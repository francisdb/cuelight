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
    "red": { "file": "blocks", "color": "#FF0000" }
  },
  "variables": { "score": 1500 },
  "layers": [
    { "name": "label", "type": "text", "font": "red", "text": "10", "x": 2, "y": 1, "align": "top_left" },
    { "name": "boxed", "type": "text", "font": "white", "text": "1", "size": [9, 8], "x": 20 },
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
    assert_eq!(NumberFormat::Plain.format(1500.0), "1500");
    assert_eq!(NumberFormat::Plain.format(2.5), "2.5");
    assert_eq!(NumberFormat::Thousands.format(0.0), "0");
    assert_eq!(NumberFormat::Thousands.format(999.0), "999");
    assert_eq!(NumberFormat::Thousands.format(412_345_000.0), "412,345,000");
    assert_eq!(NumberFormat::Thousands.format(-1234.4), "-1,234");
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
