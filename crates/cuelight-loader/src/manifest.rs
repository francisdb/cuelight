//! Show folders without a filesystem: a manifest listing a folder's files,
//! and loading a show from files held in memory.
//!
//! A browser cannot list a directory, so a site's build writes a
//! `manifest.json` into each show folder (the `cuelight-manifest` binary
//! does it). A web host fetches the manifest, fetches the files it lists
//! and hands them to [`load_from_memory`], which applies the same folder
//! conventions as [`load`](crate::load) does on disk.

use crate::{
    register_font, register_image, Driver, LoadError, IMAGE_EXTENSIONS, SOUND_EXTENSIONS,
    VECTOR_EXTENSION,
};
use cuelight::Engine;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Name of the manifest file inside a show folder.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The files of a show folder, as paths relative to it with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    /// Version of this manifest layout.
    pub format: u32,
    /// `show.json`, `test-driver.json` when present, then everything
    /// directly in `assets/`, `assets/fonts/`, `assets/sounds/` and
    /// `assets/videos/`, sorted.
    pub files: Vec<String>,
}

impl Manifest {
    /// List the files of the show folder at `dir` that loading looks at.
    pub fn for_dir(dir: impl AsRef<Path>) -> Result<Self, LoadError> {
        let dir = dir.as_ref();
        if !dir.join("show.json").is_file() {
            return Err(LoadError::NoShowDocument(dir.to_owned()));
        }
        let mut files = vec!["show.json".to_owned()];
        if dir.join("test-driver.json").is_file() {
            files.push("test-driver.json".to_owned());
        }
        for sub in ["assets", "assets/fonts", "assets/sounds", "assets/videos"] {
            let path = dir.join(sub);
            if !path.is_dir() {
                continue;
            }
            for file in crate::files_in(&path)? {
                if let Some(name) = file.file_name().and_then(|n| n.to_str()) {
                    files.push(format!("{sub}/{name}"));
                }
            }
        }
        Ok(Self { format: 1, files })
    }

    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a manifest serializes") + "\n"
    }
}

/// What [`load_from_memory`] found besides the show it loaded.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LoadedFiles {
    /// The folder's driver script (`test-driver.json`), when it has one.
    pub driver: Option<Driver>,
    /// Names of the images, fonts and vector artwork that were registered.
    pub images: Vec<String>,
    pub fonts: Vec<String>,
    pub vectors: Vec<String>,
    /// Sound files from `assets/sounds/`, for the host to decode and
    /// register; see [`Loaded::sounds`](crate::Loaded::sounds).
    pub sounds: Vec<SoundFile>,
    /// Asset files left alone because support for their format is not
    /// compiled in.
    pub skipped: Vec<String>,
}

/// A sound file of a show, undecoded: the engine registers it by
/// `name` with the duration the host's audio backend finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundFile {
    /// The file's stem, the name audio layers use.
    pub name: String,
    /// Lowercase file extension, picking the decoder.
    pub extension: String,
    pub bytes: Vec<u8>,
}

/// Load a show folder held in memory into `engine`: `files` maps paths
/// relative to the folder (`show.json`, `assets/orb.png`,
/// `assets/fonts/score.fnt`, ...) to their bytes, with the same conventions
/// as [`load`](crate::load). Paths outside those conventions are ignored.
pub fn load_from_memory(
    engine: &mut Engine,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<LoadedFiles, LoadError> {
    let asset_error = |path: &str, message: String| LoadError::Asset {
        path: PathBuf::from(path),
        message,
    };
    let text = |path: &str| -> Result<Option<&str>, LoadError> {
        files
            .get(path)
            .map(|bytes| std::str::from_utf8(bytes).map_err(|e| asset_error(path, e.to_string())))
            .transpose()
    };
    let show = text("show.json")?.ok_or_else(|| LoadError::NoShowDocument(PathBuf::new()))?;
    let mut loaded = LoadedFiles {
        driver: None,
        images: Vec::new(),
        fonts: Vec::new(),
        vectors: Vec::new(),
        sounds: Vec::new(),
        skipped: Vec::new(),
    };

    for (path, bytes) in files {
        // Directly in assets/ or assets/fonts/, nothing deeper.
        let Some((dir, file)) = path.rsplit_once('/') else {
            continue;
        };
        let (stem, extension) = match file.rsplit_once('.') {
            Some((stem, extension)) => (stem, extension.to_ascii_lowercase()),
            None => (file, String::new()),
        };
        match dir {
            "assets" if extension == VECTOR_EXTENSION => {
                #[cfg(feature = "svg")]
                {
                    crate::register_vector(engine, stem, bytes)
                        .map_err(|e| asset_error(path, e))?;
                    loaded.vectors.push(stem.to_owned());
                }
                #[cfg(not(feature = "svg"))]
                loaded.skipped.push(path.clone());
            }
            "assets" if IMAGE_EXTENSIONS.contains(&extension.as_str()) => {
                register_image(engine, stem, &extension, bytes)
                    .map_err(|e| asset_error(path, e))?;
                loaded.images.push(stem.to_owned());
            }
            "assets" => loaded.skipped.push(path.clone()),
            "assets/fonts" if matches!(extension.as_str(), "fnt" | "ttf" | "otf") => {
                if loaded.fonts.iter().any(|f| f == stem) {
                    return Err(asset_error(
                        path,
                        format!("another font in this folder is already named {stem:?}"),
                    ));
                }
                if extension == "fnt" {
                    let fnt =
                        std::str::from_utf8(bytes).map_err(|e| asset_error(path, e.to_string()))?;
                    register_font(engine, stem, fnt, |page| {
                        files.get(&format!("assets/fonts/{page}")).cloned()
                    })
                    .map_err(|e| asset_error(path, e))?;
                } else {
                    #[cfg(feature = "outline-fonts")]
                    engine
                        .set_outline_font(stem, bytes.clone())
                        .map_err(|e| asset_error(path, e.to_string()))?;
                    #[cfg(not(feature = "outline-fonts"))]
                    {
                        loaded.skipped.push(path.clone());
                        continue;
                    }
                }
                loaded.fonts.push(stem.to_owned());
            }
            "assets/sounds" if SOUND_EXTENSIONS.contains(&extension.as_str()) => {
                loaded.sounds.push(SoundFile {
                    name: stem.to_owned(),
                    extension,
                    bytes: bytes.clone(),
                });
            }
            _ => {}
        }
    }

    engine.load_show(show).map_err(|source| LoadError::Engine {
        path: PathBuf::from("show.json"),
        source,
    })?;
    if let Some(json) = text("test-driver.json")? {
        loaded.driver = Some(
            Driver::from_json(json).map_err(|message| LoadError::Driver {
                path: PathBuf::from("test-driver.json"),
                message,
            })?,
        );
    }
    Ok(loaded)
}
