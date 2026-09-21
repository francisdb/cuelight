//! Write `manifest.json` into show folders, so hosts that cannot list a
//! directory (a browser) know which files to fetch.
//!
//! ```sh
//! cuelight-manifest shows/beacon shows/minigolf
//! ```

use cuelight_loader::{Manifest, MANIFEST_FILE};

fn main() -> std::process::ExitCode {
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    if dirs.is_empty() || dirs.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("usage: cuelight-manifest <show folder>...");
        eprintln!("writes {MANIFEST_FILE} into each show folder");
        return std::process::ExitCode::from(2);
    }
    let mut failed = false;
    for dir in dirs {
        let written = Manifest::for_dir(&dir)
            .map_err(|e| e.to_string())
            .and_then(|m| {
                let path = std::path::Path::new(&dir).join(MANIFEST_FILE);
                std::fs::write(&path, m.to_json())
                    .map(|()| (path, m.files.len()))
                    .map_err(|e| e.to_string())
            });
        match written {
            Ok((path, files)) => println!("{}: {files} files", path.display()),
            Err(e) => {
                eprintln!("{dir}: {e}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
