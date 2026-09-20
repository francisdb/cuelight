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
