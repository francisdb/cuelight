//! The checked-in JSON Schema must match what the model types generate.
//!
//! Regenerate after model changes:
//! `UPDATE_SCHEMA=1 cargo test --features schema --test schema`

#![cfg(feature = "schema")]

use cuelight::Show;

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schemas/show.schema.json");

#[test]
fn checked_in_schema_matches_model() {
    let schema = schemars::schema_for!(Show);
    let generated = serde_json::to_string_pretty(&schema).unwrap() + "\n";
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::write(PATH, &generated).unwrap();
        return;
    }
    // Normalize CRLF in case the checkout converted line endings.
    let checked_in = std::fs::read_to_string(PATH)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert_eq!(
        checked_in, generated,
        "schemas/show.schema.json is out of date; regenerate with \
         UPDATE_SCHEMA=1 cargo test --features schema --test schema"
    );
}

#[test]
fn example_shows_validate_as_shows() {
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/shows")).unwrap()
    {
        let mut path = entry.unwrap().path();
        // A directory is a show folder: its document is show.json inside.
        if path.is_dir() {
            path = path.join("show.json");
            assert!(path.is_file(), "show folder without show.json: {path:?}");
        }
        // Driver files (*test-driver.json) script the player, they are
        // not shows.
        if path.to_string_lossy().ends_with("test-driver.json") {
            continue;
        }
        let json = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str::<Show>(&json)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
    }
}
