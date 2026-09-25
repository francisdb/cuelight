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
    assert_eq!(
        loaded.driver,
        Some(Driver::from_file(shows().join("beacon/test-driver.json")).unwrap())
    );
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
    assert_eq!(
        loaded.driver,
        Some(Driver::from_file(shows().join("minigolf.test-driver.json")).unwrap())
    );
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
    assert_eq!(in_memory.driver, on_disk.driver);
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

#[cfg(feature = "pack")]
#[test]
fn a_packed_show_loads_like_its_folder() {
    let dir = std::env::temp_dir().join(format!("cuelight-loader-pack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("beacon.cuelight");
    let files = cuelight_loader::pack(shows().join("beacon"), &out).unwrap();
    assert!(files >= 2, "{files}");

    let mut engine = Engine::new();
    let loaded = cuelight_loader::load(&mut engine, &out).unwrap();
    assert_eq!(loaded.images, ["orb"]);
    assert_eq!(loaded.show, out);
    assert!(loaded.driver.is_some());
    assert!(engine.show().is_some());

    // The same bytes, unpacked, are the folder's files.
    let unpacked = cuelight_loader::read_pack(&out).unwrap();
    assert!(unpacked.contains_key("show.json"));
    assert!(unpacked.contains_key("assets/orb.png"));
    assert_eq!(
        unpacked["show.json"],
        std::fs::read(shows().join("beacon/show.json")).unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "pack")]
#[test]
fn a_zip_wrapping_the_folder_unpacks_too() {
    use std::io::Write;
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.add_directory("beacon/", options).unwrap();
        writer.start_file("beacon/show.json", options).unwrap();
        writer
            .write_all(b"{ \"name\": \"b\", \"size\": [8, 8] }")
            .unwrap();
        writer.finish().unwrap();
    }
    let files = cuelight_loader::unpack(&cursor.into_inner()).unwrap();
    assert_eq!(files.keys().collect::<Vec<_>>(), ["show.json"]);
    assert!(cuelight_loader::unpack(b"not a zip").is_err());
}

/// A show dropped into a folder of media that was already arranged the
/// way its author wanted: no assets/ folder, nothing copied or linked.
#[cfg(all(feature = "png", any(feature = "outline-fonts", feature = "pack")))]
fn beside_media(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cuelight-loader-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("Intro")).unwrap();
    std::fs::create_dir_all(dir.join("Amazonia")).unwrap();
    std::fs::create_dir_all(dir.join("Fonts")).unwrap();
    // Two folders reusing one filename, which flattening could not keep
    // apart without renaming.
    for folder in ["Intro", "Amazonia"] {
        std::fs::write(
            dir.join(folder).join("01.png"),
            one_pixel(folder == "Intro"),
        )
        .unwrap();
    }
    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../cuelight/tests/fonts/cuelight_test_sans.ttf"
    );
    std::fs::copy(font, dir.join("Fonts/Title.ttf")).unwrap();
    std::fs::write(
        dir.join("show.json"),
        r##"{
          "name": "in place", "size": [32, 32],
          "fonts": { "title": { "file": "Fonts/Title.ttf", "size": 8 } },
          "layers": [
            { "name": "a", "type": "image", "image": "Intro/01.png" },
            { "name": "b", "type": "image", "image": "Amazonia/01.png" },
            { "name": "c", "type": "text", "font": "title", "text": "hi" }
          ]
        }"##,
    )
    .unwrap();
    dir
}

/// A 1x1 PNG, red or blue, so the two folders' files differ.
#[cfg(all(feature = "png", any(feature = "outline-fonts", feature = "pack")))]
fn one_pixel(red: bool) -> Vec<u8> {
    let rgba = if red {
        [255, 0, 0, 255]
    } else {
        [0, 0, 255, 255]
    };
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&rgba).unwrap();
    }
    out
}

#[cfg(all(feature = "png", feature = "outline-fonts"))]
#[test]
fn a_show_can_name_media_where_it_already_lies() {
    let dir = beside_media("beside");
    let mut engine = Engine::new();
    let loaded = load(&mut engine, &dir).unwrap();

    // Registered under the names the document used, so two folders
    // reusing a filename stay apart without anything being renamed.
    let mut images = loaded.images.clone();
    images.sort();
    assert_eq!(images, ["Amazonia/01.png", "Intro/01.png"]);
    assert!(engine.image("Intro/01.png").is_some());
    assert!(engine.image("Amazonia/01.png").is_some());
    assert_ne!(
        engine.image("Intro/01.png").unwrap().pixels,
        engine.image("Amazonia/01.png").unwrap().pixels,
        "the same filename in two folders is two different files"
    );
    assert_eq!(loaded.fonts, ["Fonts/Title.ttf"]);
    assert!(loaded.skipped.is_empty(), "{:?}", loaded.skipped);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_path_that_climbs_out_of_the_show_is_refused() {
    let dir = std::env::temp_dir().join(format!("cuelight-loader-escape-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("show.json"),
        r##"{ "name": "escape", "size": [8, 8], "layers": [
              { "name": "a", "type": "image", "image": "../secrets/key.png" }] }"##,
    )
    .unwrap();
    let mut engine = Engine::new();
    let err = load(&mut engine, &dir).unwrap_err().to_string();
    assert!(err.contains("climbs out"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_named_file_that_is_not_there_fails_the_load() {
    let dir = std::env::temp_dir().join(format!("cuelight-loader-absent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("show.json"),
        r##"{ "name": "absent", "size": [8, 8], "layers": [
              { "name": "a", "type": "image", "image": "art/missing.png" }] }"##,
    )
    .unwrap();
    let mut engine = Engine::new();
    // A path is a claim about the filesystem; a false one is a broken
    // show, and better found now than when the layer should have drawn.
    let err = load(&mut engine, &dir).unwrap_err().to_string();
    assert!(err.contains("art/missing.png"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_stem_nobody_registered_is_still_no_error() {
    let dir = std::env::temp_dir().join(format!("cuelight-loader-stem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("show.json"),
        r##"{ "name": "later", "size": [8, 8], "layers": [
              { "name": "a", "type": "image", "image": "streamed" }] }"##,
    )
    .unwrap();
    // A stem is only a name, which a host may satisfy whenever it likes:
    // a streamed or generated asset must keep working.
    let mut engine = Engine::new();
    let loaded = load(&mut engine, &dir).unwrap();
    assert!(loaded.skipped.is_empty(), "{:?}", loaded.skipped);
    assert!(engine.image("streamed").is_none());
    engine.set_image("streamed", 1, 1, vec![255; 4]).unwrap();
    assert!(engine.image("streamed").is_some());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(feature = "pack")]
#[test]
fn packing_refuses_a_show_that_names_a_file_that_is_not_there() {
    let dir = std::env::temp_dir().join(format!("cuelight-loader-packbad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("show.json"),
        r##"{ "name": "absent", "size": [8, 8], "layers": [
              { "name": "a", "type": "image", "image": "art/missing.png" },
              { "name": "b", "type": "image", "image": "art/gone.png" }] }"##,
    )
    .unwrap();
    let out = dir.join("absent.cuelight");
    let err = cuelight_loader::pack(&dir, &out).unwrap_err().to_string();
    // Both at once: hunting them one attempt at a time is no way to find out.
    assert!(err.contains("art/missing.png"), "{err}");
    assert!(err.contains("art/gone.png"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn clips_named_by_path_reach_the_host_as_paths() {
    let dir = std::env::temp_dir().join(format!("cuelight-loader-clips-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("Intro")).unwrap();
    // Never opened here: a clip is the host's to decode, so the loader
    // only has to say where it is.
    std::fs::write(dir.join("Intro/opening.mp4"), b"not really a clip").unwrap();
    std::fs::write(
        dir.join("show.json"),
        r##"{ "name": "clips", "size": [8, 8], "layers": [
              { "name": "a", "type": "video", "video": "Intro/opening.mp4" }] }"##,
    )
    .unwrap();
    let mut engine = Engine::new();
    let loaded = load(&mut engine, &dir).unwrap();
    assert_eq!(loaded.videos, [dir.join("Intro/opening.mp4")]);
    assert!(loaded.skipped.is_empty(), "{:?}", loaded.skipped);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(all(feature = "png", feature = "pack"))]
#[test]
fn a_packed_show_carries_the_media_it_names_by_path() {
    let dir = beside_media("packed");
    let out = std::env::temp_dir().join(format!("cuelight-beside-{}.cuelight", std::process::id()));
    let _ = std::fs::remove_file(&out);
    cuelight_loader::pack(&dir, &out).unwrap();

    // Packing a show written against media beside it has to take that
    // media with it, or the pack cannot load.
    let packed = cuelight_loader::read_pack(&out).unwrap();
    for name in ["Intro/01.png", "Amazonia/01.png", "Fonts/Title.ttf"] {
        assert!(
            packed.contains_key(name),
            "{name} missing from {:?}",
            packed.keys()
        );
    }

    let mut engine = Engine::new();
    let loaded = cuelight_loader::load(&mut engine, &out).unwrap();
    let mut images = loaded.images.clone();
    images.sort();
    assert_eq!(images, ["Amazonia/01.png", "Intro/01.png"]);
    assert!(engine.image("Intro/01.png").is_some());
    assert!(loaded.skipped.is_empty(), "{:?}", loaded.skipped);

    std::fs::remove_file(&out).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn seeking_lands_where_playing_there_would_have() {
    let show = r##"{
      "name": "seek", "size": [64, 64],
      "layers": [{ "name": "box", "type": "shape", "x": 0,
                   "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF",
                   "timelines": [{ "name": "slide", "autoplay": true,
                     "tracks": [{ "property": "x",
                                  "keys": [{ "t": 0, "v": 0 }, { "t": 4, "v": 40 }] }] }] }]
    }"##;
    let at = |engine: &Engine| engine.resolved_layers().unwrap()[0].transform.0[4];

    let mut played = Engine::new();
    played.load_show(show).unwrap();
    for _ in 0..120 {
        played.advance_frame(1.0 / 60.0);
    }

    // Seeking from cold, and seeking backwards from further on, both land
    // where playing there would have.
    let mut sought = Engine::new();
    sought.load_show(show).unwrap();
    cuelight_loader::seek(&mut sought, None, 2.0, 60.0);
    assert!(
        (at(&sought) - at(&played)).abs() < 1e-9,
        "{} vs {}",
        at(&sought),
        at(&played)
    );

    for _ in 0..120 {
        sought.advance_frame(1.0 / 60.0);
    }
    cuelight_loader::seek(&mut sought, None, 2.0, 60.0);
    assert!(
        (at(&sought) - at(&played)).abs() < 1e-9,
        "going back is the same place"
    );
    assert!((sought.time() - 2.0).abs() < 1e-9, "{}", sought.time());
}

#[test]
fn seeking_replays_the_driver_on_the_way() {
    let show = r##"{ "name": "driven", "size": [8, 8], "variables": { "score": 0 },
      "layers": [{ "name": "b", "type": "shape", "shape": { "rect": [0, 0, 8, 8] },
                   "fill": "#FFFFFF" }] }"##;
    let driver = Driver::from_json(
        r#"{ "steps": [{ "set": { "score": 10 } }, { "wait": 1 },
                       { "set": { "score": 20 } }, { "wait": 1 },
                       { "set": { "score": 30 } }] }"#,
    )
    .unwrap();
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();

    cuelight_loader::seek(&mut engine, Some(driver.clone()), 0.5, 60.0);
    assert_eq!(
        engine.variable("score"),
        Some(&cuelight::Value::Number(10.0))
    );
    cuelight_loader::seek(&mut engine, Some(driver.clone()), 1.5, 60.0);
    assert_eq!(
        engine.variable("score"),
        Some(&cuelight::Value::Number(20.0))
    );
    // And back: the driver is replayed from the top, not rewound.
    cuelight_loader::seek(&mut engine, Some(driver), 0.5, 60.0);
    assert_eq!(
        engine.variable("score"),
        Some(&cuelight::Value::Number(10.0))
    );
}

/// A show and its files, held in memory, for the cases where the layout
/// matters more than the content.
fn in_memory(show: &str, files: &[(&str, &[u8])]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut all: std::collections::BTreeMap<String, Vec<u8>> =
        [("show.json".to_owned(), show.as_bytes().to_vec())]
            .into_iter()
            .collect();
    for (path, bytes) in files {
        all.insert((*path).to_owned(), bytes.to_vec());
    }
    all
}

#[test]
fn a_file_the_loader_does_not_walk_is_reported_not_dropped() {
    // assets/art/ is a folder the loader does not look in, so the file
    // registers nothing. Saying so beats a missing-asset error later
    // that points at the name rather than the file sitting right there.
    let show = r##"{ "name": "deep", "size": [8, 8], "layers": [] }"##;
    let files = in_memory(
        show,
        &[
            ("assets/art/logo.png", b"not really a png"),
            ("assets/sounds/deeper/beep.wav", b"not really a wav"),
        ],
    );
    let mut engine = Engine::new();
    let loaded = cuelight_loader::load_from_memory(&mut engine, &files).unwrap();
    assert_eq!(
        loaded.skipped,
        ["assets/art/logo.png", "assets/sounds/deeper/beep.wav"]
    );
}

#[cfg(feature = "png")]
#[test]
fn a_file_the_show_names_by_path_is_not_reported() {
    // The same folder the loader does not walk, but this time the
    // document asks for the file by path, so it is used, not skipped.
    let show = r##"{ "name": "named", "size": [8, 8], "layers": [
        { "name": "logo", "type": "image", "image": "art/logo.png" } ] }"##;
    let png = std::fs::read(shows().join("beacon/assets/orb.png")).unwrap();
    let files = in_memory(show, &[("art/logo.png", &png)]);
    let mut engine = Engine::new();
    let loaded = cuelight_loader::load_from_memory(&mut engine, &files).unwrap();
    assert!(loaded.skipped.is_empty(), "{:?}", loaded.skipped);
    assert_eq!(loaded.images, ["art/logo.png"]);
}

#[test]
fn clips_are_not_reported_since_the_host_opens_them() {
    let show = r##"{ "name": "clips", "size": [8, 8], "layers": [] }"##;
    let files = in_memory(
        show,
        &[("assets/videos/intro/clip.mp4", b"not really an mp4")],
    );
    let mut engine = Engine::new();
    let loaded = cuelight_loader::load_from_memory(&mut engine, &files).unwrap();
    assert!(loaded.skipped.is_empty(), "{:?}", loaded.skipped);
}
