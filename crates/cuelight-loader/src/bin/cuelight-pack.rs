//! Pack a show folder into one `.cuelight` file, a zip of the folder's
//! contents that the player and the loader open like the folder itself.
//!
//! ```sh
//! cuelight-pack shows/beacon              # writes shows/beacon.cuelight
//! cuelight-pack shows/beacon out/b.cuelight
//! ```

use cuelight_loader::PACK_EXTENSION;
use std::path::{Path, PathBuf};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = || {
        eprintln!("usage: cuelight-pack <show folder> [output.{PACK_EXTENSION}]");
        std::process::ExitCode::from(2)
    };
    let (dir, out) = match args.as_slice() {
        [dir] => {
            let dir = Path::new(dir);
            let name = dir
                .file_name()
                .map(|n| n.to_os_string())
                .unwrap_or_else(|| "show".into());
            let mut out = dir.parent().map_or_else(PathBuf::new, Path::to_path_buf);
            out.push(name);
            out.set_extension(PACK_EXTENSION);
            (dir.to_path_buf(), out)
        }
        [dir, out] if !dir.starts_with('-') => (PathBuf::from(dir), PathBuf::from(out)),
        _ => return usage(),
    };
    match cuelight_loader::pack(&dir, &out) {
        Ok(files) => {
            println!("{}: {files} files", out.display());
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {e}", dir.display());
            std::process::ExitCode::FAILURE
        }
    }
}
