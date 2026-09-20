//! Outline fonts: layout from font metrics, resolved as glyph runs.

#![cfg(feature = "outline-fonts")]

use cuelight::{Engine, ResolvedShape};

const FONT: &[u8] = include_bytes!("fonts/cuelight_test_sans.ttf");

// The test font: 1000 units per em, ascent 1069, descent -293, advances
// A 639, 1 572, space 260. At size 100 a unit is 0.1 px.
const SHOW: &str = r##"{
  "name": "outline", "size": [800, 400],
  "fonts": {
    "big": { "file": "sans", "size": 100, "color": "#FF8000" },
    "edged": { "file": "sans", "size": 50, "border": { "color": "#000000", "width": 2 } }
  },
  "variables": { "speed": 11 },
  "layers": [
    { "name": "a", "type": "text", "font": "big", "text": "A1", "x": 10, "y": 20, "align": "top_left" },
    { "name": "boxed", "type": "text", "font": "big", "text": "1", "size": [200, 300], "x": 400 },
    { "name": "speed", "type": "text", "font": "edged", "text": "0", "x": 780, "y": 380,
      "anchor": "bottom_right", "bindings": [{ "property": "text", "variable": "speed" }] },
    { "name": "lines", "type": "text", "font": "big", "text": "A\n1", "align": "right", "size": [700, 300] }
  ]
}"##;

fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_outline_font("sans", FONT).unwrap();
    engine.load_show(SHOW).unwrap();
    engine
}

type Run = (f64, Vec<(f64, f64)>, Option<([u8; 4], f64)>, [u8; 4]);

fn run(engine: &Engine, name: &str) -> Run {
    let layers = engine.resolved_layers().unwrap();
    let layer = layers.iter().find(|l| l.name == name).unwrap();
    match &layer.shape {
        ResolvedShape::GlyphRun {
            size,
            glyphs,
            border,
            ..
        } => (
            *size,
            glyphs.iter().map(|g| (g.x, g.y)).collect(),
            *border,
            layer.color,
        ),
        other => panic!("{name}: {other:?}"),
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn glyphs_sit_on_the_baseline_and_advance() {
    let (size, glyphs, border, color) = run(&engine(), "a");
    assert_eq!(size, 100.0);
    assert_eq!(border, None);
    assert_eq!(color, [255, 128, 0, 255]);
    // origin x = 10, then A's advance; baseline = y + ascent
    assert!(
        close(glyphs[0].0, 10.0) && close(glyphs[1].0, 10.0 + 63.9),
        "{glyphs:?}"
    );
    assert!(close(glyphs[0].1, 20.0 + 106.9), "{glyphs:?}");
}

#[test]
fn text_centers_in_its_box() {
    let (_, glyphs, _, _) = run(&engine(), "boxed");
    // "1" is 57.2 wide and one line (136.2) tall, centered in 200x300 at x = 400
    assert!(
        close(glyphs[0].0, 400.0 + (200.0 - 57.2) / 2.0),
        "{glyphs:?}"
    );
    assert!(
        close(glyphs[0].1, (300.0 - 136.2) / 2.0 + 106.9),
        "{glyphs:?}"
    );
}

#[test]
fn anchor_and_bindings_work_like_bitmap_text() {
    let mut engine = engine();
    let (size, glyphs, border, _) = run(&engine, "speed");
    assert_eq!((size, border), (50.0, Some(([0, 0, 0, 255], 2.0))));
    // "11" at size 50 is 57.2 wide; its box's bottom-right is (780, 380)
    assert!(close(glyphs[0].0, 780.0 - 57.2), "{glyphs:?}");
    assert!(close(glyphs[0].1, 380.0 - 68.1 + 53.45), "{glyphs:?}");
    engine.set_variable("speed", 111.0);
    let (_, glyphs, _, _) = run(&engine, "speed");
    assert_eq!(glyphs.len(), 3);
    assert!(close(glyphs[0].0, 780.0 - 85.8), "{glyphs:?}");
}

#[test]
fn lines_align_on_their_own() {
    let (_, glyphs, _, _) = run(&engine(), "lines");
    // right aligned in 700: "A" ends at 700, so does "1", one line lower
    assert!(
        close(glyphs[0].0, 700.0 - 63.9) && close(glyphs[1].0, 700.0 - 57.2),
        "{glyphs:?}"
    );
    assert!(close(glyphs[1].1 - glyphs[0].1, 136.2), "{glyphs:?}");
}

#[test]
fn size_must_match_the_kind_of_font() {
    let mut engine = Engine::new();
    engine.set_outline_font("sans", FONT).unwrap();
    let no_size = SHOW.replace(r#""size": 100, "#, "");
    assert!(engine.load_show(&no_size).is_err());
    assert!(engine
        .load_show(&SHOW.replace(r#""size": 100"#, r#""size": 0"#))
        .is_err());
    // unknown fonts are not judged: they may still be registered, as either kind
    let mut engine = Engine::new();
    engine.load_show(&no_size).unwrap();
    assert!(engine.resolved_layers().unwrap().is_empty());
}

#[test]
fn garbage_is_not_a_font() {
    let mut engine = Engine::new();
    assert!(engine.set_outline_font("sans", vec![1u8, 2, 3]).is_err());
    assert!(!engine.has_font("sans"));
}
