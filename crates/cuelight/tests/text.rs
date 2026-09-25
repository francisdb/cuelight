//! Text layers: bitmap font registration, layout, text bindings.

use cuelight::{BitmapFont, Engine, NumberFormat, ResolvedShape};

// A 3-pixel-wide font: every digit and ',' is a 2x3 block, advance 3.
const FNT: &str = r#"info face="Blocks" size=3
common lineHeight=4 base=3 scaleW=2 scaleH=3 pages=1
page id=0 file="blocks_0.png"
char id=32 x=0 y=0 width=0 height=0 xoffset=0 yoffset=0 xadvance=3 page=0
char id=44 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=48 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=49 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=53 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
"#;

const SHOW: &str = r##"{
  "name": "text",
  "size": [32, 8],
  "fonts": {
    "white": { "file": "blocks" },
    "red": { "file": "blocks", "color": "#FF0000" },
    "cast": { "file": "blocks", "color": "#FFFFFF",
              "shadow": { "color": "#000000", "offset": [1, 1] } }
  },
  "variables": { "score": 1500 },
  "layers": [
    { "name": "label", "type": "text", "font": "red", "text": "10", "x": 2, "y": 1, "align": "top_left" },
    { "name": "boxed", "type": "text", "font": "white", "text": "1", "size": [9, 8], "x": 20 },
    { "name": "cast", "type": "text", "font": "cast", "text": "1", "x": 4, "y": 2, "align": "top_left" },
    {
      "name": "score",
      "type": "text",
      "font": "white",
      "text": "0",
      "bindings": [{ "property": "text", "variable": "score", "format": "thousands" }]
    }
  ]
}"##;

fn engine() -> Engine {
    let mut engine = Engine::new();
    let font = BitmapFont::parse(FNT).unwrap();
    assert_eq!(font.pages(), ["blocks_0.png"]);
    engine
        .set_font("blocks", font, vec![(2, 3, vec![255; 2 * 3 * 4])])
        .unwrap();
    engine.load_show(SHOW).unwrap();
    engine
}

fn bitmap(engine: &Engine, name: &str) -> (f64, f64, f64, f64, [u8; 4]) {
    let layers = engine.resolved_layers().unwrap();
    let layer = layers
        .iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("{name} not resolved"));
    match &layer.shape {
        ResolvedShape::Bitmap {
            image,
            x,
            y,
            width,
            height,
        } => {
            let px = &image.pixels[..4];
            (*x, *y, *width, *height, [px[0], px[1], px[2], px[3]])
        }
        other => panic!("{name} should be a bitmap, got {other:?}"),
    }
}

/// The text a layer resolves to, without drawing it.
fn shown(engine: &Engine, name: &str) -> String {
    engine
        .values()
        .unwrap()
        .into_iter()
        .find(|(layer, prop, _)| layer == name && *prop == cuelight::Property::Text)
        .map(|(_, _, v)| v.to_text())
        .unwrap_or_else(|| panic!("no text on layer {name:?}"))
}

#[test]
fn text_rasterizes_tinted_at_layer_position() {
    let engine = engine();
    // "10": glyphs at 0 and 3, 2 wide -> 5x3 bitmap
    assert_eq!(
        bitmap(&engine, "label"),
        (2.0, 1.0, 5.0, 3.0, [255, 0, 0, 255])
    );
}

#[test]
fn text_centers_in_its_box_by_default() {
    let engine = engine();
    // "1" measures 3x4; centered in 9x8 -> offset (3, 2)
    let (x, y, w, h, _) = bitmap(&engine, "boxed");
    assert_eq!((x, y, w, h), (23.0, 2.0, 2.0, 3.0));
}

#[test]
fn text_binding_formats_numbers() {
    let mut engine = engine();
    // "1,500" is 5 glyphs, the last at 12 -> 14 wide
    assert_eq!(bitmap(&engine, "score").2, 14.0);
    engine.set_variable("score", 5.0);
    assert_eq!(bitmap(&engine, "score").2, 2.0);
    engine.set_variable("score", "0 5");
    assert_eq!(bitmap(&engine, "score").2, 8.0);
}

#[test]
fn text_without_registered_font_is_skipped() {
    let mut engine = Engine::new();
    engine.load_show(SHOW).unwrap();
    assert!(engine.resolved_layers().unwrap().is_empty());
    assert!(!engine.has_font("blocks"));
}

#[test]
fn undeclared_font_style_is_rejected() {
    let mut engine = Engine::new();
    let show = SHOW.replace(r#""font": "red""#, r#""font": "blue""#);
    assert!(engine.load_show(&show).is_err());
}

#[test]
fn keyframing_text_is_rejected() {
    let mut engine = Engine::new();
    let show = SHOW.replace(
        r#""align": "top_left" }"#,
        r#""align": "top_left", "timelines": [{ "name": "t", "tracks": [{ "property": "text", "keys": [] }] }] }"#,
    );
    assert!(engine.load_show(&show).is_err());
}

#[test]
fn page_count_must_match() {
    let mut engine = Engine::new();
    let font = BitmapFont::parse(FNT).unwrap();
    assert!(engine.set_font("blocks", font, vec![]).is_err());
}

#[test]
fn number_formats() {
    assert_eq!(NumberFormat::Plain.format(1500.0, None), "1500");
    assert_eq!(NumberFormat::Plain.format(2.5, None), "2.5");
    assert_eq!(NumberFormat::Thousands.format(0.0, None), "0");
    assert_eq!(NumberFormat::Thousands.format(999.0, None), "999");
    assert_eq!(
        NumberFormat::Thousands.format(412_345_000.0, None),
        "412,345,000"
    );
    assert_eq!(NumberFormat::Thousands.format(-1234.4, None), "-1,234");
}

const MAPPED: &str = r##"{
  "name": "mapped",
  "size": [32, 8],
  "fonts": {
    "dim": { "file": "blocks", "color": "#404040" },
    "lit": { "file": "blocks" }
  },
  "variables": { "player": 1, "mode": "attract" },
  "layers": [
    {
      "name": "p2",
      "type": "text",
      "font": "dim",
      "text": "0",
      "bindings": [
        { "property": "font", "variable": "player", "map": { "2": "lit" }, "default": "dim" },
        { "property": "opacity", "variable": "mode", "map": { "game": 1 }, "default": 0.25 }
      ]
    }
  ]
}"##;

#[test]
fn mapped_bindings_pick_values_by_variable() {
    let mut engine = Engine::new();
    let font = BitmapFont::parse(FNT).unwrap();
    engine
        .set_font("blocks", font, vec![(2, 3, vec![255; 2 * 3 * 4])])
        .unwrap();
    engine.load_show(MAPPED).unwrap();
    let state = |e: &Engine| {
        let layers = e.resolved_layers().unwrap();
        let color = bitmap(e, "p2").4;
        (color, layers[0].opacity)
    };
    assert_eq!(state(&engine), ([64, 64, 64, 255], 0.25));
    engine.set_variable("player", 2.0);
    engine.set_variable("mode", "game");
    assert_eq!(state(&engine), ([255, 255, 255, 255], 1.0));
}

#[test]
fn font_binding_to_undeclared_style_is_rejected() {
    let mut engine = Engine::new();
    let show = MAPPED.replace(r#""2": "lit""#, r#""2": "bright""#);
    assert!(engine.load_show(&show).is_err());
}

#[test]
fn properties_a_layer_does_not_have_are_rejected() {
    let mut engine = Engine::new();
    for binding in ["text", "font"] {
        let show = format!(
            r##"{{ "name": "s", "size": [8, 8], "layers": [
                {{ "name": "box", "type": "shape", "shape": {{ "rect": [0, 0, 1, 1] }}, "fill": "#FFFFFF",
                  "bindings": [{{ "property": "{binding}", "variable": "v" }}] }} ] }}"##
        );
        assert!(engine.load_show(&show).is_err(), "{binding} on a shape");
    }
}

#[test]
fn anchor_places_the_text_box() {
    let show = SHOW.replace(
        r#""x": 2, "y": 1, "align": "top_left" }"#,
        r#""x": 32, "y": 8, "anchor": "bottom_right" }"#,
    );
    let mut engine = engine();
    engine.load_show(&show).unwrap();
    // "10" measures 6x4: its box's bottom-right corner lands on (32, 8).
    let (x, y, w, h, _) = bitmap(&engine, "label");
    assert_eq!((x, y, w, h), (26.0, 4.0, 5.0, 3.0));
}

#[test]
fn a_changing_number_does_not_re_rasterize_static_text() {
    let mut engine = engine();
    let revision = |e: &Engine, name: &str| {
        let layers = e.resolved_layers().unwrap();
        match &layers.iter().find(|l| l.name == name).unwrap().shape {
            ResolvedShape::Bitmap { image, .. } => image.revision(),
            other => panic!("{other:?}"),
        }
    };
    let label = revision(&engine, "label");
    // Far more distinct strings than the old entry-count bound allowed.
    for n in 0..2000u32 {
        // The test font only has the glyphs 0, 1 and 5: count in base 3.
        let digits = |mut n: u32| {
            let mut out = String::new();
            loop {
                out.insert(0, ['0', '1', '5'][(n % 3) as usize]);
                n /= 3;
                if n == 0 {
                    break out;
                }
            }
        };
        engine.set_variable("score", digits(n).as_str());
        revision(&engine, "score");
        revision(&engine, "label");
    }
    assert_eq!(revision(&engine, "label"), label);
}

#[test]
fn a_bitmap_shadow_is_a_second_drawing_behind_the_text() {
    let layers = engine().resolved_layers().unwrap();
    let cast: Vec<_> = layers.iter().filter(|l| l.name == "cast").collect();
    assert_eq!(cast.len(), 2, "a shadow is a draw of its own");
    let at = |layer: &cuelight::ResolvedLayer| match &layer.shape {
        ResolvedShape::Bitmap { x, y, image, .. } => (*x, *y, image.pixels.clone()),
        other => panic!("{other:?}"),
    };
    let (sx, sy, dark) = at(cast[0]);
    let (tx, ty, light) = at(cast[1]);
    assert_eq!((sx - tx, sy - ty), (1.0, 1.0), "moved by the offset");
    // A raster has no color to tint at draw time, so the shadow is its own
    // rasterization: the same glyphs in the shadow color.
    assert_eq!(&light[..4], [255, 255, 255, 255], "the text");
    assert_eq!(&dark[..4], [0, 0, 0, 255], "its shadow");
    assert_eq!(dark.len(), light.len());
}

#[test]
fn decimals_round_and_always_show() {
    assert_eq!(NumberFormat::Plain.format(1.4833333, Some(1)), "1.5");
    assert_eq!(NumberFormat::Plain.format(1.5, Some(2)), "1.50");
    assert_eq!(NumberFormat::Plain.format(1.5, Some(0)), "2");
    // A value that rounds to nothing keeps no sign from where it came.
    assert_eq!(NumberFormat::Plain.format(-0.04, Some(1)), "0.0");
    assert_eq!(NumberFormat::Plain.format(-1.26, Some(1)), "-1.3");
}

#[test]
fn decimals_go_with_thousands() {
    assert_eq!(NumberFormat::Thousands.format(1500.0, Some(2)), "1,500.00");
    assert_eq!(
        NumberFormat::Thousands.format(-1234.567, Some(1)),
        "-1,234.6"
    );
    assert_eq!(NumberFormat::Thousands.format(999.95, Some(1)), "1,000.0");
    assert_eq!(
        NumberFormat::Thousands.format(412_345_000.0, Some(0)),
        "412,345,000"
    );
}

#[test]
fn a_bound_number_shows_the_decimals_it_asks_for() {
    let show = r##"{ "name": "d", "size": [64, 32],
      "fonts": { "plain": { "file": "none" } },
      "variables": { "speed": 0 },
      "layers": [{ "name": "readout", "type": "text", "text": "0", "font": "plain",
        "bindings": [{ "property": "text", "variable": "speed", "decimals": 1 }] }] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine.set_variable("speed", 1.4833333333333334);
    engine.advance_to(0.0);
    assert_eq!(shown(&engine, "readout"), "1.5");
}

#[test]
fn too_many_decimals_is_refused() {
    let show = r##"{ "name": "d", "size": [64, 32],
      "fonts": { "plain": { "file": "none" } },
      "variables": { "speed": 0 },
      "layers": [{ "name": "readout", "type": "text", "text": "0", "font": "plain",
        "bindings": [{ "property": "text", "variable": "speed", "decimals": 40 }] }] }"##;
    assert!(Engine::new().load_show(show).is_err());
}

const WORDED: &str = r##"{ "name": "d", "size": [64, 32],
  "fonts": { "plain": { "file": "none" } },
  "variables": { "progress": 40, "multiplier": 2.5, "ball": 2, "mode": "attract" },
  "layers": [
    { "name": "percent", "type": "text", "text": "", "font": "plain",
      "bindings": [{ "property": "text", "variable": "progress", "suffix": "%" }] },
    { "name": "times", "type": "text", "text": "", "font": "plain",
      "bindings": [{ "property": "text", "variable": "multiplier", "decimals": 1,
                     "suffix": " X" }] },
    { "name": "ball", "type": "text", "text": "", "font": "plain",
      "bindings": [{ "property": "text", "variable": "ball", "prefix": "BALL " }] },
    { "name": "mode", "type": "text", "text": "READY", "font": "plain",
      "bindings": [{ "property": "text", "variable": "mode", "map": { "play": "PLAY" },
                     "prefix": "> ", "suffix": " <" }] }
  ] }"##;

#[test]
fn words_go_round_a_bound_value() {
    let mut engine = Engine::new();
    engine.load_show(WORDED).unwrap();
    assert_eq!(shown(&engine, "percent"), "40%");
    assert_eq!(shown(&engine, "times"), "2.5 X");
    assert_eq!(shown(&engine, "ball"), "BALL 2");
    // A map with nothing to say does not apply, so the base text stays
    // and takes no words.
    assert_eq!(shown(&engine, "mode"), "READY");
    // Mapped text gets them as much as a number does.
    engine.set_variable("mode", "play");
    assert_eq!(shown(&engine, "mode"), "> PLAY <");
}

#[test]
fn a_counting_transition_leaves_the_words_still() {
    let show = WORDED.replace(
        r#""variable": "ball", "prefix": "BALL ""#,
        r#""variable": "ball", "prefix": "BALL ", "transition": { "duration": 1 }"#,
    );
    let mut engine = Engine::new();
    engine.load_show(&show).unwrap();
    engine.advance_to(1.0);
    assert_eq!(shown(&engine, "ball"), "BALL 2");
    engine.set_variable("ball", 4.0);
    engine.advance_to(1.5);
    assert_eq!(shown(&engine, "ball"), "BALL 3");
    engine.advance_to(2.0);
    assert_eq!(shown(&engine, "ball"), "BALL 4");
}

#[test]
fn words_on_a_property_that_is_not_text_are_reported() {
    let show = WORDED.replace(
        r#"{ "property": "text", "variable": "progress", "suffix": "%" }"#,
        r#"{ "property": "x", "variable": "progress", "suffix": "%" }"#,
    );
    let mut engine = Engine::new();
    engine.load_show(&show).unwrap();
    let warnings = engine.load_warnings().join("\n");
    assert!(warnings.contains("words round its value"), "{warnings}");
}
