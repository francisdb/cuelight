//! The checked-in JSON Schema must match what the model types generate.
//!
//! Regenerate after model changes:
//! `UPDATE_SCHEMA=1 cargo test --features schema --test schema`

#![cfg(feature = "schema")]

use cuelight::Scene;

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schemas/scene.schema.json");

#[test]
fn checked_in_schema_matches_model() {
    let schema = schemars::schema_for!(Scene);
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
        "schemas/scene.schema.json is out of date; regenerate with \
         UPDATE_SCHEMA=1 cargo test --features schema --test schema"
    );
}

#[test]
fn example_scenes_validate_as_scenes() {
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/scenes")).unwrap()
    {
        let path = entry.unwrap().path();
        // Driver files (<scene>.driver.json) script the player, they are
        // not scenes.
        if path.to_string_lossy().ends_with(".driver.json") {
            continue;
        }
        let json = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str::<Scene>(&json)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
    }
}
