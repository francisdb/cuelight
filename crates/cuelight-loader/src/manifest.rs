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
    VECTOR_EXTENSION, VIDEO_EXTENSIONS,
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
    /// `show.json`, `test-driver.json` when present, everything directly
    /// in `assets/`, `assets/fonts/`, `assets/sounds/` and
    /// `assets/videos/`, and then every file the document names by a path
    /// of its own, wherever beside the document that is. The last group is
    /// what makes a packed show carry the media it was written against.
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
        // Everything under assets/, at any depth. The loader only reads
        // the folders it knows, but a file it does not read still has to
        // be seen: a clip grouped in a subfolder is opened, and anything
        // nothing can use is reported rather than dropped.
        if dir.join("assets").is_dir() {
            files.extend(crate::files_under(&dir.join("assets"), "assets")?);
        }
        // Then whatever the document names by path, which may be anywhere
        // beside it. Without these a packed show would be missing exactly
        // the media it was written against.
        let show = std::fs::read_to_string(dir.join("show.json")).unwrap_or_default();
        for (_, name, _) in named_files(&show) {
            if dir.join(&name).is_file() && !files.contains(&name) {
                files.push(name);
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
    /// compiled in. A build problem, not a show problem.
    pub skipped: Vec<String>,
    /// Font families the show's artwork asked for and the show does not
    /// ship, whose text was not drawn. A show problem, and the same
    /// wherever it plays.
    pub missing_fonts: Vec<String>,
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
        missing_fonts: Vec::new(),
    };
    // Artwork is converted once every font is registered, whichever
    // order the files came in: its text is drawn with the show's own
    // fonts, and a folder listing does not put them first.
    #[cfg(feature = "svg")]
    let mut artwork: Vec<(String, Vec<u8>)> = Vec::new();

    let mut unclaimed: Vec<String> = Vec::new();
    // The pages a bitmap font read, which are its to use rather than
    // left over, wherever the loop happened to meet them.
    let mut pages: Vec<String> = Vec::new();
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
                artwork.push((stem.to_owned(), bytes.clone()));
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
                        let path = format!("assets/fonts/{page}");
                        let bytes = files.get(&path).cloned();
                        if bytes.is_some() {
                            pages.push(path);
                        }
                        bytes
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
            // Under assets/ but not somewhere the loader looks: a
            // folder deeper than it walks, or one it does not know.
            // Collected rather than dropped, and reported below once the
            // document has had its say, since it may name the file by
            // path itself. Clips are the caller's to collect.
            dir if dir.starts_with("assets/")
                && dir != "assets/videos"
                && !dir.starts_with("assets/videos/") =>
            {
                unclaimed.push(path.clone());
            }
            _ => {}
        }
    }

    #[cfg(feature = "svg")]
    let named = register_named(engine, show, files, &mut loaded, &mut artwork)?;
    #[cfg(not(feature = "svg"))]
    let named = register_named(engine, show, files, &mut loaded)?;
    loaded.skipped.extend(
        unclaimed
            .into_iter()
            .filter(|p| !named.contains(p) && !pages.contains(p)),
    );

    // Now that every font is in, the artwork: its text is drawn with
    // them.
    #[cfg(feature = "svg")]
    {
        let fonts = crate::SvgFonts::of(engine);
        for (name, bytes) in artwork {
            let missing = crate::register_vector(engine, &name, &bytes, &fonts)
                .map_err(|e| asset_error(&name, e))?;
            for family in missing {
                if !loaded.missing_fonts.contains(&family) {
                    loaded.missing_fonts.push(family);
                }
            }
            loaded.vectors.push(name);
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

/// Register the assets the document names by path rather than by stem.
///
/// The conventional folders are scanned whether anything uses them or
/// not; a path is the other way round, and the document decides.
///
/// A path that points at nothing fails the load. A path is a claim about
/// the filesystem, and a false one is a broken show; a stem is only a
/// name, which a host may satisfy whenever it likes through `set_image`
/// and its siblings, so an unregistered stem stays no error at all. That
/// is what keeps a streamed or generated asset working while a mistyped
/// path does not ship.
/// Register the files the document names by a path of its own, and
/// return the paths it registered, so the caller only reports a file
/// nothing wanted.
fn register_named(
    engine: &mut Engine,
    show: &str,
    files: &BTreeMap<String, Vec<u8>>,
    loaded: &mut LoadedFiles,
    #[cfg(feature = "svg")] artwork: &mut Vec<(String, Vec<u8>)>,
) -> Result<Vec<String>, LoadError> {
    let Ok(parsed) = serde_json::from_str::<cuelight::Show>(show) else {
        // Not a show at all; loading it will say so properly in a moment.
        return Ok(Vec::new());
    };
    let asset_error = |path: &str, message: String| LoadError::Asset {
        path: PathBuf::from(path),
        message,
    };
    let (mut done, mut missing): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for (kind, name) in references(&parsed) {
        // Clips are handed to the host as paths, never read here.
        if kind == Asset::Video || !is_path(&name) || done.contains(&name) {
            continue;
        }
        safe_path(&name).map_err(|message| asset_error(&name, message))?;
        done.push(name.clone());
        let Some(bytes) = files.get(&name) else {
            // Collected rather than raised here: all of them at once beats
            // one per attempt.
            missing.push(name);
            continue;
        };
        let extension = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match kind {
            Asset::Image => {
                register_image(engine, &name, &extension, bytes)
                    .map_err(|e| asset_error(&name, e))?;
                loaded.images.push(name);
            }
            Asset::Vector => {
                #[cfg(feature = "svg")]
                artwork.push((name, bytes.clone()));
                #[cfg(not(feature = "svg"))]
                loaded.skipped.push(name);
            }
            Asset::Sound => loaded.sounds.push(SoundFile {
                name,
                extension,
                bytes: bytes.clone(),
            }),
            Asset::Font => {
                if extension == "fnt" {
                    let fnt = std::str::from_utf8(bytes)
                        .map_err(|e| asset_error(&name, e.to_string()))?;
                    let folder = name.rsplit_once('/').map_or("", |(dir, _)| dir);
                    register_font(engine, &name, fnt, |page| {
                        let beside = if folder.is_empty() {
                            page.to_owned()
                        } else {
                            format!("{folder}/{page}")
                        };
                        files.get(&beside).cloned()
                    })
                    .map_err(|e| asset_error(&name, e))?;
                } else {
                    #[cfg(feature = "outline-fonts")]
                    engine
                        .set_outline_font(&name, bytes.clone())
                        .map_err(|e| asset_error(&name, e.to_string()))?;
                    #[cfg(not(feature = "outline-fonts"))]
                    {
                        loaded.skipped.push(name);
                        continue;
                    }
                }
                loaded.fonts.push(name);
            }
            Asset::Video => unreachable!("clips are the host's to open"),
        }
    }
    if !missing.is_empty() {
        return Err(LoadError::Asset {
            path: PathBuf::from(missing.join(", ")),
            message: format!("{} file(s) the show names are not there", missing.len()),
        });
    }
    Ok(done)
}

/// What kind of asset a name in a show document refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asset {
    Image,
    Vector,
    Sound,
    Video,
    Font,
}

/// Whether a name in a show document is a path to a file rather than the
/// stem of something registered under one of the conventional folders.
///
/// A stem never carries an extension and never has a separator in it, so
/// either mark says the document means a file where it lies.
pub(crate) fn is_path(name: &str) -> bool {
    if name.contains('/') {
        return true;
    }
    name.rsplit_once('.').is_some_and(|(stem, extension)| {
        let extension = extension.to_ascii_lowercase();
        !stem.is_empty()
            && (IMAGE_EXTENSIONS.contains(&extension.as_str())
                || SOUND_EXTENSIONS.contains(&extension.as_str())
                || VIDEO_EXTENSIONS.contains(&extension.as_str())
                || extension == VECTOR_EXTENSION
                || matches!(extension.as_str(), "fnt" | "ttf" | "otf"))
    })
}

/// A path a show may name: relative, inside the show's own folder, and
/// with nothing that could climb out of it.
///
/// Refused rather than sanitized. A document that asks for something
/// outside its folder is wrong about where it is, and quietly reading a
/// different file than it named would be worse than saying no.
pub(crate) fn safe_path(name: &str) -> Result<&str, String> {
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(format!("{name:?} is absolute"));
    }
    if name.contains('\\') {
        return Err(format!(
            "{name:?} uses backslashes; paths are '/' separated"
        ));
    }
    if name.len() >= 2 && name.as_bytes()[1] == b':' {
        return Err(format!("{name:?} names a drive"));
    }
    if name.split('/').any(|part| part == ".." || part == ".") {
        return Err(format!("{name:?} climbs out of the show's folder"));
    }
    Ok(name)
}

/// Every asset a show document names, with the kind it is used as.
///
/// The document decides what a show needs, which is what lets one sit
/// beside a folder of a few hundred clips and load the handful it uses.
pub(crate) fn references(show: &cuelight::Show) -> Vec<(Asset, String)> {
    use cuelight::{LayerKind, ReelCells};
    let mut out: Vec<(Asset, String)> = show
        .fonts
        .values()
        .map(|style| (Asset::Font, style.file.clone()))
        .collect();
    fn walk(layers: &[cuelight::Layer], out: &mut Vec<(Asset, String)>) {
        for layer in layers {
            match &layer.kind {
                LayerKind::Image { image, .. } => out.push((Asset::Image, image.clone())),
                LayerKind::Vector { vector, .. } => out.push((Asset::Vector, vector.clone())),
                LayerKind::Video { video, .. } => {
                    out.extend(video.iter().map(|n| (Asset::Video, n.to_owned())));
                }
                LayerKind::Audio { sound, .. } => {
                    out.extend(sound.iter().map(|n| (Asset::Sound, n.to_owned())));
                }
                LayerKind::Digits {
                    display: cuelight::DigitDisplay::Reel(reel),
                    ..
                } => match &reel.cells {
                    Some(ReelCells::Vectors(names)) => {
                        out.extend(names.iter().map(|n| (Asset::Vector, n.clone())));
                    }
                    Some(ReelCells::Images(names)) => {
                        out.extend(names.iter().map(|n| (Asset::Image, n.clone())));
                    }
                    _ => {}
                },
                _ => {}
            }
            walk(layer.children(), out);
        }
    }
    for layers in show.layer_trees() {
        walk(layers, &mut out);
    }
    out
}

/// The files a show document names by path, with the kind each is used
/// as and, for a bitmap font, the pages named beside it.
///
/// Used before anything is read, to decide what to read at all.
pub(crate) fn named_files(show: &str) -> Vec<(Asset, String, Vec<String>)> {
    let Ok(parsed) = serde_json::from_str::<cuelight::Show>(show) else {
        return Vec::new();
    };
    let mut out: Vec<(Asset, String, Vec<String>)> = Vec::new();
    for (kind, name) in references(&parsed) {
        if !is_path(&name) || safe_path(&name).is_err() {
            continue;
        }
        if out.iter().any(|(_, seen, _)| *seen == name) {
            continue;
        }
        out.push((kind, name, Vec::new()));
    }
    out
}
