//! Host-side loading for [`cuelight`] shows.
//!
//! The engine does no I/O: it takes a show document plus images and fonts a
//! host registers. This crate is that host work, shared so every host does
//! not write it again:
//!
//! - [`load`]: a show folder (or a loose show file) from disk into an engine.
//! - [`register_image`] / [`register_font`]: the same from bytes, for hosts
//!   without a filesystem (web, embedded assets).
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
//!     fonts/            bitmap fonts: .fnt plus its page images,
//!                       registered by .fnt filename stem
//! ```
//!
//! Where bytes come from and how they become pixels are separate concerns.
//! Image formats are decoded by extension in [`decode_image`], each behind
//! a cargo feature (`png`, on by default); a host that decodes images
//! itself can turn them off and still use the folder and driver handling.

mod driver;

pub use driver::{Driver, DriverPlayer, Step};

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
    /// Files under `assets/` that were left alone because no decoder for
    /// their extension is compiled in; hosts may want to log them.
    pub skipped: Vec<PathBuf>,
}

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
                loaded.fonts = register_font_dir(engine, &font_dir)?;
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

/// Register every `*.fnt` directly in `dir` as a bitmap font named by its
/// filename stem; page images are read from `dir` as the description
/// names them. Returns the names.
pub fn register_font_dir(engine: &mut Engine, dir: &Path) -> Result<Vec<String>, LoadError> {
    let mut names = Vec::new();
    for path in files_in(dir)? {
        if extension(&path) != "fnt" {
            continue;
        }
        let name = stem(&path);
        let fnt = read_to_string(&path)?;
        register_font(engine, &name, &fnt, |page| {
            std::fs::read(dir.join(page)).ok()
        })
        .map_err(|message| LoadError::Asset {
            path: path.clone(),
            message,
        })?;
        names.push(name);
    }
    Ok(names)
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
fn files_in(dir: &Path) -> Result<Vec<PathBuf>, LoadError> {
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
