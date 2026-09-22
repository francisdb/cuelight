use cuelight::Engine;
use cuelight_loader::{load, Driver, DriverPlayer, LoadError, Step};
use std::path::PathBuf;

fn shows() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../cuelight/examples/shows"
    ))
}

#[cfg(feature = "png")]
#[test]
fn loads_a_show_folder_with_assets_and_driver() {
    let mut engine = Engine::new();
    let loaded = load(&mut engine, shows().join("beacon")).unwrap();
    assert_eq!(loaded.images, ["orb"]);
    assert!(loaded.fonts.is_empty());
    assert!(loaded.skipped.is_empty());
    assert!(loaded.show.ends_with("beacon/show.json"));
    assert!(loaded
        .driver
        .as_ref()
        .unwrap()
        .ends_with("beacon/test-driver.json"));
    assert!(engine.image("orb").is_some());
    // the orb image resolves, so its asset really was registered first
    let layers = engine.resolved_layers().unwrap();
    assert!(layers
        .iter()
        .any(|l| matches!(l.shape, cuelight::ResolvedShape::Image { .. })));
}

#[test]
fn loads_a_loose_show_and_finds_its_driver() {
    let mut engine = Engine::new();
    let loaded = load(&mut engine, shows().join("minigolf.json")).unwrap();
    assert!(loaded
        .driver
        .unwrap()
        .ends_with("minigolf.test-driver.json"));
    let loaded = load(&mut engine, shows().join("slideshow.json")).unwrap();
    assert_eq!(loaded.driver, None);
}

#[test]
fn errors_name_the_path() {
    let mut engine = Engine::new();
    let err = load(&mut engine, shows()).unwrap_err();
    assert!(matches!(err, LoadError::NoShowDocument(_)), "{err}");
    let err = load(&mut engine, shows().join("nope.json")).unwrap_err();
    assert!(err.to_string().contains("nope.json"), "{err}");
}

#[test]
fn driver_applies_steps_as_time_passes() {
    let driver = Driver::from_json(
        r#"{ "loop": true, "steps": [
            { "set": { "score": 5 } }, { "wait": 1.0 }, { "trigger": "go" }, { "wait": 0.5 } ] }"#,
    )
    .unwrap();
    assert_eq!(driver.duration(), 1.5);
    let mut engine = Engine::new();
    load(&mut engine, shows().join("minigolf.json")).unwrap();
    let mut player = DriverPlayer::new(driver);
    let applied = player.advance(&mut engine, 0.25);
    assert!(matches!(applied.as_slice(), [Step::Set { .. }]));
    assert_eq!(engine.variable("score").unwrap().as_number(), 5.0);
    assert!(player.advance(&mut engine, 0.5).is_empty());
    // crossing the 1.0s wait fires the trigger
    let applied = player.advance(&mut engine, 0.5);
    assert_eq!(
        applied,
        [Step::Trigger {
            trigger: "go".into()
        }]
    );
    // and after the last wait the script starts over
    let applied = player.advance(&mut engine, 0.5);
    assert!(matches!(applied.as_slice(), [Step::Set { .. }]));
    assert!(!player.is_done());
}

#[test]
fn a_looping_driver_without_waits_stops_instead_of_spinning() {
    let driver = Driver::from_json(r#"{ "loop": true, "steps": [{ "trigger": "go" }] }"#).unwrap();
    let mut engine = Engine::new();
    let mut player = DriverPlayer::new(driver);
    assert_eq!(player.advance(&mut engine, 1.0).len(), 2);
    assert!(player.is_done());
}

#[cfg(not(feature = "png"))]
#[test]
fn without_a_decoder_images_are_skipped_not_fatal() {
    let mut engine = Engine::new();
    let loaded = load(&mut engine, shows().join("beacon")).unwrap();
    assert!(loaded.images.is_empty());
    assert_eq!(loaded.skipped.len(), 1);
    assert!(cuelight_loader::decode_image("png", &[]).is_err());
}

#[test]
fn unknown_image_formats_are_reported() {
    let err = cuelight_loader::decode_image("TIFF", &[]).unwrap_err();
    assert!(err.contains(".tiff"), "{err}");
}

/// A throwaway show folder holding the engine's test font as `sans.ttf`.
fn outline_show(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cuelight-loader-{tag}-{}", std::process::id()));
    let fonts = dir.join("assets/fonts");
    std::fs::create_dir_all(&fonts).unwrap();
    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../cuelight/tests/fonts/cuelight_test_sans.ttf"
    );
    std::fs::copy(font, fonts.join("sans.ttf")).unwrap();
    std::fs::write(
        dir.join("show.json"),
        r##"{ "name": "o", "size": [200, 100],
             "fonts": { "big": { "file": "sans", "size": 40 } },
             "layers": [{ "name": "t", "type": "text", "font": "big", "text": "A1" }] }"##,
    )
    .unwrap();
    dir
}

#[cfg(feature = "outline-fonts")]
#[test]
fn registers_outline_fonts_by_stem() {
    let dir = outline_show("ttf");
    let mut engine = Engine::new();
    let loaded = load(&mut engine, &dir).unwrap();
    assert_eq!(loaded.fonts, ["sans"]);
    assert!(loaded.skipped.is_empty());
    assert!(matches!(
        engine.resolved_layers().unwrap()[0].shape,
        cuelight::ResolvedShape::GlyphRun { .. }
    ));
    // a second font with the same stem makes the style ambiguous
    std::fs::copy(
        dir.join("assets/fonts/sans.ttf"),
        dir.join("assets/fonts/sans.otf"),
    )
    .unwrap();
    let err = load(&mut Engine::new(), &dir).unwrap_err();
    assert!(err.to_string().contains("already named"), "{err}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(not(feature = "outline-fonts"))]
#[test]
fn without_the_feature_outline_fonts_are_skipped() {
    let dir = outline_show("skip");
    let mut engine = Engine::new();
    let loaded = load(&mut engine, &dir).unwrap();
    assert!(loaded.fonts.is_empty());
    assert_eq!(loaded.skipped.len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Read a show folder into memory the way a web host would: the manifest
/// first, then the files it lists.
#[cfg_attr(not(any(feature = "png", feature = "outline-fonts")), allow(dead_code))]
fn fetch(dir: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let manifest = cuelight_loader::Manifest::for_dir(dir).unwrap();
    let round_trip = cuelight_loader::Manifest::from_json(&manifest.to_json()).unwrap();
    assert_eq!(round_trip, manifest);
    manifest
        .files
        .iter()
        .map(|f| (f.clone(), std::fs::read(dir.join(f)).unwrap()))
        .collect()
}

#[test]
fn manifest_lists_what_loading_looks_at() {
    let manifest = cuelight_loader::Manifest::for_dir(shows().join("beacon")).unwrap();
    assert_eq!(
        manifest.files,
        ["show.json", "test-driver.json", "assets/orb.png"]
    );
    assert!(cuelight_loader::Manifest::for_dir(shows()).is_err());
}

#[cfg(feature = "png")]
#[test]
fn loading_from_memory_matches_loading_from_disk() {
    let dir = shows().join("beacon");
    let (mut from_disk, mut from_memory) = (Engine::new(), Engine::new());
    let on_disk = load(&mut from_disk, &dir).unwrap();
    let in_memory = cuelight_loader::load_from_memory(&mut from_memory, &fetch(&dir)).unwrap();
    assert_eq!(in_memory.images, on_disk.images);
    assert_eq!(in_memory.fonts, on_disk.fonts);
    assert!(in_memory.skipped.is_empty());
    assert_eq!(
        in_memory.driver,
        Some(Driver::from_file(on_disk.driver.unwrap()).unwrap())
    );
    assert_eq!(
        from_memory.resolved_layers().unwrap(),
        from_disk.resolved_layers().unwrap()
    );
}

#[cfg(feature = "outline-fonts")]
#[test]
fn outline_fonts_load_from_memory_too() {
    let dir = outline_show("memory");
    let mut engine = Engine::new();
    let loaded = cuelight_loader::load_from_memory(&mut engine, &fetch(&dir)).unwrap();
    assert_eq!(loaded.fonts, ["sans"]);
    assert_eq!(loaded.driver, None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn memory_without_a_show_document_is_an_error() {
    let files = std::collections::BTreeMap::from([("assets/x.png".to_owned(), vec![])]);
    let err = cuelight_loader::load_from_memory(&mut Engine::new(), &files).unwrap_err();
    assert!(matches!(err, LoadError::NoShowDocument(_)), "{err}");
}

#[cfg(feature = "svg")]
#[test]
fn converts_an_svg_into_vector_artwork() {
    use cuelight::{PathElement, ResolvedShape};
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 10">
        <g transform="translate(2 0)">
          <rect x="0" y="0" width="4" height="10" fill="#ff0000"/>
        </g>
        <circle cx="15" cy="5" r="3" fill="none" stroke="#0000ff" stroke-width="2"/>
        <rect x="0" y="0" width="1" height="1" fill="#00ff00" opacity="0.5"/>
      </svg>"##;
    let mut engine = Engine::new();
    cuelight_loader::register_vector(&mut engine, "art", svg).unwrap();
    let art = engine.vector("art").unwrap();
    assert_eq!((art.width, art.height), (20.0, 10.0));
    assert_eq!(art.paths.len(), 3);
    // The group's transform is applied to the rect.
    assert_eq!(art.paths[0].fill, Some([255, 0, 0, 255]));
    assert_eq!(art.paths[0].elements[0], PathElement::MoveTo([2.0, 0.0]));
    // An outline-only circle keeps its stroke and no fill.
    assert_eq!(art.paths[1].fill, None);
    assert_eq!(art.paths[1].stroke, Some(([0, 0, 255, 255], 2.0)));
    // Opacity lands in the alpha.
    assert_eq!(art.paths[2].fill, Some([0, 255, 0, 128]));

    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "show.json".to_owned(),
        br#"{ "name": "svg", "size": [40, 20], "layers": [
            { "name": "art", "type": "vector", "vector": "art", "size": [40, 20] } ] }"#
            .to_vec(),
    );
    files.insert("assets/art.svg".to_owned(), svg.to_vec());
    let loaded = cuelight_loader::load_from_memory(&mut engine, &files).unwrap();
    assert_eq!(loaded.vectors, ["art"]);
    let layers = engine.resolved_layers().unwrap();
    assert_eq!(layers.len(), 3);
    let ResolvedShape::Path { elements, .. } = &layers[0].shape else {
        panic!("{:?}", layers[0].shape);
    };
    assert_eq!(elements[0], PathElement::MoveTo([4.0, 0.0]));
}
