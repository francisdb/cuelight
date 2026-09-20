//! Show format version and reporting of fields the engine ignored.

use cuelight::{Engine, Error, FORMAT};

#[test]
fn a_show_without_format_is_format_1() {
    let mut engine = Engine::new();
    engine
        .load_show(r#"{ "name": "s", "size": [8, 8] }"#)
        .unwrap();
    assert_eq!(engine.show().unwrap().format, 1);
    assert!(engine.load_warnings().is_empty());
}

#[test]
fn newer_formats_are_refused_before_anything_else() {
    let mut engine = Engine::new();
    // Even content this engine could not parse at all.
    let show = format!(
        r#"{{ "format": {}, "name": "s", "size": [8, 8],
             "layers": [{{ "name": "x", "type": "hologram" }}] }}"#,
        FORMAT + 1
    );
    match engine.load_show(&show) {
        Err(Error::UnsupportedFormat { found, supported }) => {
            assert_eq!((found, supported), (u64::from(FORMAT) + 1, FORMAT));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn nonsense_formats_are_invalid() {
    let mut engine = Engine::new();
    for format in ["0", "\"one\"", "1.5", "-1"] {
        let show = format!(r#"{{ "format": {format}, "name": "s", "size": [8, 8] }}"#);
        assert!(
            matches!(engine.load_show(&show), Err(Error::InvalidShow(_))),
            "format {format}"
        );
    }
}

#[test]
fn ignored_fields_are_reported_by_path() {
    let show = r##"{
      "$schema": "../schemas/show.schema.json",
      "format": 1, "name": "s", "size": [8, 8], "backgruond": "#101010",
      "layers": [
        { "name": "a", "type": "shape", "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF",
          "colour": "#FF0000",
          "timelines": [{ "name": "t", "tracks": [], "autoPlay": true }] },
        { "name": "g", "type": "group", "children": [
            { "name": "b", "type": "shape", "shape": { "circle": [0, 0, 1] }, "fill": "#FFFFFF", "z": 3 } ] }
      ],
      "scenes": [{ "name": "one", "triger": "go" }]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let mut warnings = engine.load_warnings().to_vec();
    warnings.sort();
    assert_eq!(
        warnings,
        [
            "backgruond",
            "layers[0].colour",
            "layers[0].timelines[0].autoPlay",
            "layers[1].children[0].z",
            "scenes[0].triger",
        ]
    );
    // A clean reload clears them.
    engine
        .load_show(r#"{ "name": "s", "size": [8, 8] }"#)
        .unwrap();
    assert!(engine.load_warnings().is_empty());
}

#[test]
fn bundled_example_shows_load_without_warnings() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/shows");
    for entry in std::fs::read_dir(dir).unwrap() {
        let mut path = entry.unwrap().path();
        if path.is_dir() {
            path = path.join("show.json");
        }
        if path.to_string_lossy().ends_with("test-driver.json") {
            continue;
        }
        let mut engine = Engine::new();
        engine
            .load_show(&std::fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            engine.load_warnings(),
            &[] as &[String],
            "{}",
            path.display()
        );
    }
}
