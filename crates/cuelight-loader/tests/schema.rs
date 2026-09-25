//! The checked-in driver schema must match what the model generates.
//!
//! Regenerate after model changes:
//! `UPDATE_SCHEMA=1 cargo test -p cuelight-loader --features schema --test schema`

#![cfg(feature = "schema")]

use cuelight_loader::Driver;

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schemas/driver.schema.json");

#[test]
fn checked_in_schema_matches_model() {
    let schema = schemars::schema_for!(Driver);
    let generated = serde_json::to_string_pretty(&schema).unwrap() + "\n";
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::write(PATH, &generated).unwrap();
        return;
    }
    let checked_in = std::fs::read_to_string(PATH)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert_eq!(
        checked_in, generated,
        "schemas/driver.schema.json is out of date; regenerate with \
         UPDATE_SCHEMA=1 cargo test -p cuelight-loader --features schema --test schema"
    );
}

#[test]
fn the_example_drivers_validate() {
    let shows = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../cuelight/examples/shows"
    ));
    let mut seen = 0;
    for entry in std::fs::read_dir(&shows).unwrap() {
        let path = entry.unwrap().path();
        let driver = if path.is_dir() {
            path.join("test-driver.json")
        } else {
            continue;
        };
        if !driver.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&driver).unwrap();
        Driver::from_json(&text).unwrap_or_else(|e| panic!("{}: {e}", driver.display()));
        seen += 1;
    }
    assert!(
        seen > 0,
        "no driver scripts found under {}",
        shows.display()
    );
}
