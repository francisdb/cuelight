//! Host-side loading for [`cuelight`] shows.
//!
//! The engine does no I/O: it takes a show document plus images and fonts a
//! host registers. This crate is that host work, shared so every host does
//! not write it again:
//!
//! - [`load`]: a show folder (or a loose show file) from disk into an engine.
//! - [`load_from_memory`]: the same folder conventions over files held in
//!   memory, for hosts without a filesystem (web, embedded assets), with
//!   [`Manifest`] telling them which files a show folder has.
//! - [`register_image`] / [`register_font`]: single assets from bytes.
//! - [`Driver`]: scripted triggers and variable changes with delays, the
//!   `test-driver.json` convention, standing in for a live host.
//!
//! A show folder:
//!
//! ```text
//! myshow/
//!   show.json           the show document (required)
//!   test-driver.json    optional driver script
//!   assets/             images, registered by filename stem
//!     fonts/            fonts, registered by filename stem: bitmap (.fnt
//!                       plus its page images) or outline (.ttf, .otf)
//!     sounds/           sound files, listed for the host's audio backend
//! ```
//!
//! Where bytes come from and how they become pixels are separate concerns.
//! Image formats are decoded by extension in [`decode_image`], each behind
//! a cargo feature (`png`, on by default); a host that decodes images
//! itself can turn them off and still use the folder and driver handling.
//! Outline fonts are a feature too (`outline-fonts`, on by default).

mod driver;
mod manifest;

pub use driver::{Driver, DriverPlayer, Step};
pub use manifest::{load_from_memory, LoadedFiles, Manifest, MANIFEST_FILE};

use cuelight::{BitmapFont, Engine};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("show folder {0} has no show.json")]
    NoShowDocument(PathBuf),
    #[error("{path}: {message}")]
    Asset { path: PathBuf, message: String },
    #[error("{path}: {source}")]
    Engine {
        path: PathBuf,
        source: cuelight::Error,
    },
    #[error("{path}: {message}")]
    Driver { path: PathBuf, message: String },
}

/// What [`load`] found besides the show it loaded into the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Loaded {
    /// The show document that was loaded.
    pub show: PathBuf,
    /// The driver script to play with it, when there is one: a folder's
    /// `test-driver.json`, or `<show>.test-driver.json` next to a loose
    /// show file.
    pub driver: Option<PathBuf>,
    /// Names of the images and fonts that were registered.
    pub images: Vec<String>,
    pub fonts: Vec<String>,
    /// Sound files found in `assets/sounds/` (see [`SOUND_EXTENSIONS`]),
    /// for the host to decode and register by their stem with
    /// `Engine::set_sound` and its audio backend (the `cuelight-audio`
    /// crate does both). The engine only needs their durations, so nothing
    /// here decodes them; an audio layer whose sound arrives later plays
    /// silently until then.
    pub sounds: Vec<PathBuf>,
    /// Image and font files under `assets/` that were left alone because
    /// support for their format is not compiled in; hosts may want to log
    /// them.
    pub skipped: Vec<PathBuf>,
}

/// File extensions (lowercase) of the sound files a show folder may hold
/// in `assets/sounds/`: what the `cuelight-audio` crate decodes.
pub const SOUND_EXTENSIONS: &[&str] = &["wav", "flac", "ogg", "mp3"];

/// Load a show into `engine` from `path`: a show folder, or a loose show
/// file. Assets are registered before the show loads. Fields the engine
/// ignored are available from `engine.load_warnings()` afterwards.
pub fn load(engine: &mut Engine, path: impl AsRef<Path>) -> Result<Loaded, LoadError> {
    let path = path.as_ref();
    let mut loaded = Loaded {
        show: PathBuf::new(),
        driver: None,
        images: Vec::new(),
        fonts: Vec::new(),
        sounds: Vec::new(),
        skipped: Vec::new(),
    };
    if path.is_dir() {
        loaded.show = path.join("show.json");
        if !loaded.show.is_file() {
            return Err(LoadError::NoShowDocument(path.to_owned()));
        }
        let assets = path.join("assets");
        if assets.is_dir() {
            (loaded.images, loaded.skipped) = register_image_dir(engine, &assets)?;
            let font_dir = assets.join("fonts");
            if font_dir.is_dir() {
                let (fonts, skipped) = register_font_dir(engine, &font_dir)?;
                loaded.fonts = fonts;
                loaded.skipped.extend(skipped);
            }
            let sound_dir = assets.join("sounds");
            if sound_dir.is_dir() {
                loaded.sounds = files_in(&sound_dir)?
                    .into_iter()
                    .filter(|p| SOUND_EXTENSIONS.contains(&extension(p).as_str()))
                    .collect();
            }
        }
        loaded.driver = Some(path.join("test-driver.json")).filter(|p| p.is_file());
    } else {
        loaded.show = path.to_owned();
        loaded.driver = Some(path.with_extension("test-driver.json")).filter(|p| p.is_file());
    }
    let json = read_to_string(&loaded.show)?;
    engine
        .load_show(&json)
        .map_err(|source| LoadError::Engine {
            path: loaded.show.clone(),
            source,
        })?;
    Ok(loaded)
}

/// Register every image file directly in `dir` under its filename stem,
/// in name order. Returns the names, and the files skipped because no
/// decoder for their extension is compiled in.
pub fn register_image_dir(
    engine: &mut Engine,
    dir: &Path,
) -> Result<(Vec<String>, Vec<PathBuf>), LoadError> {
    let (mut names, mut skipped) = (Vec::new(), Vec::new());
    for path in files_in(dir)? {
        let extension = extension(&path);
        if !IMAGE_EXTENSIONS.contains(&extension.as_str()) {
            skipped.push(path);
            continue;
        }
        let name = stem(&path);
        let bytes = read(&path)?;
        register_image(engine, &name, &extension, &bytes).map_err(|message| LoadError::Asset {
            path: path.clone(),
            message,
        })?;
        names.push(name);
    }
    Ok((names, skipped))
}

/// Register every font directly in `dir` under its filename stem: `*.fnt`
/// bitmap fonts (page images are read from `dir` as the description names
/// them) and `*.ttf` / `*.otf` outline fonts. Returns the names, and the
/// outline font files skipped because the `outline-fonts` feature is off.
/// Two fonts sharing a stem are an error: a style could mean either.
pub fn register_font_dir(
    engine: &mut Engine,
    dir: &Path,
) -> Result<(Vec<String>, Vec<PathBuf>), LoadError> {
    let mut names: Vec<String> = Vec::new();
    // Only filled when outline font support is compiled out.
    #[cfg_attr(feature = "outline-fonts", allow(unused_mut))]
    let mut skipped: Vec<PathBuf> = Vec::new();
    for path in files_in(dir)? {
        let extension = extension(&path);
        if !matches!(extension.as_str(), "fnt" | "ttf" | "otf") {
            continue;
        }
        let name = stem(&path);
        let asset_error = |message: String| LoadError::Asset {
            path: path.clone(),
            message,
        };
        if names.contains(&name) {
            return Err(asset_error(format!(
                "another font in this folder is already named {name:?}"
            )));
        }
        if extension == "fnt" {
            let fnt = read_to_string(&path)?;
            register_font(engine, &name, &fnt, |page| {
                std::fs::read(dir.join(page)).ok()
            })
            .map_err(asset_error)?;
        } else {
            #[cfg(feature = "outline-fonts")]
            engine
                .set_outline_font(&name, read(&path)?)
                .map_err(|e| asset_error(e.to_string()))?;
            #[cfg(not(feature = "outline-fonts"))]
            {
                skipped.push(path);
                continue;
            }
        }
        names.push(name);
    }
    Ok((names, skipped))
}

/// Decode image `bytes` of the format `extension` names (`"png"`) and
/// register them as image `name`.
pub fn register_image(
    engine: &mut Engine,
    name: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let (width, height, rgba) = decode_image(extension, bytes)?;
    engine
        .set_image(name, width, height, rgba)
        .map_err(|e| e.to_string())
}

/// Parse a BMFont text description and register it as font `name`.
/// `page` returns the bytes of a page image given the file name the
/// description uses for it; its extension picks the decoder.
pub fn register_font(
    engine: &mut Engine,
    name: &str,
    fnt: &str,
    mut page: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Result<(), String> {
    let font = BitmapFont::parse(fnt)?;
    let pages = font
        .pages()
        .iter()
        .map(|file| {
            let bytes = page(file).ok_or_else(|| format!("missing font page {file:?}"))?;
            decode_image(&extension(Path::new(file)), &bytes)
                .map_err(|e| format!("font page {file:?}: {e}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    engine
        .set_font(name, font, pages)
        .map_err(|e| e.to_string())
}

/// File extensions (lowercase) [`decode_image`] has a decoder for in this
/// build.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    #[cfg(feature = "png")]
    "png",
];

/// Decode image `bytes` into tightly packed RGBA8 `(width, height,
/// pixels)`, picking the decoder by file `extension` (case-insensitive).
pub fn decode_image(extension: &str, bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    match extension.to_ascii_lowercase().as_str() {
        #[cfg(feature = "png")]
        "png" => decode_png(bytes),
        other => {
            let _ = bytes;
            Err(format!("no decoder compiled in for .{other} images"))
        }
    }
}

/// Decode a PNG of any bit depth and color type into tightly packed RGBA8
/// `(width, height, pixels)`; missing alpha becomes opaque.
#[cfg(feature = "png")]
pub fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or("output buffer size overflow")?
    ];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[l, a]| [l, l, l, a])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&l| [l, l, l, 255]).collect(),
        other => return Err(format!("unsupported color type {other:?}")),
    };
    Ok((info.width, info.height, rgba))
}

/// The files directly in `dir`, in name order.
pub(crate) fn files_in(dir: &Path) -> Result<Vec<PathBuf>, LoadError> {
    let entries = std::fs::read_dir(dir).map_err(|source| LoadError::Io {
        path: dir.to_owned(),
        source,
    })?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    Ok(paths)
}

/// Lowercase extension, empty when there is none.
fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read(path: &Path) -> Result<Vec<u8>, LoadError> {
    std::fs::read(path).map_err(|source| LoadError::Io {
        path: path.to_owned(),
        source,
    })
}

fn read_to_string(path: &Path) -> Result<String, LoadError> {
    std::fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.to_owned(),
        source,
    })
}
