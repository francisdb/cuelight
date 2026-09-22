//! Reel rows: cells rolling through a ring of characters.

use cuelight::{BitmapFont, Engine, ResolvedShape};

// A 3-pixel-wide font: every character is a 2x3 block, advance 3.
const FNT: &str = r#"info face="Blocks" size=3
common lineHeight=4 base=3 scaleW=2 scaleH=3 pages=1
page id=0 file="blocks_0.png"
char id=48 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=49 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=50 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=51 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=52 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=53 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=54 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=55 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=56 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=57 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=65 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
"#;

/// A three-cell reel row, 30 wide and 10 high, bound to `score`.
fn show(reel: &str) -> String {
    format!(
        r##"{{ "name": "reel", "size": [64, 16],
              "fonts": {{ "cell": {{ "file": "blocks" }} }},
              "variables": {{ "score": "0" }},
              "layers": [ {{ "name": "odometer", "type": "digits", "digits": 3,
                             "size": [30, 10], "justify": "right",
                             "bindings": [ {{ "property": "text", "variable": "score" }} ],
                             "display": {{ "reel": {reel} }} }} ] }}"##
    )
}

fn engine(reel: &str) -> Engine {
    let mut engine = Engine::new();
    let font = BitmapFont::parse(FNT).unwrap();
    engine
        .set_font("blocks", font, vec![(2, 3, vec![255; 2 * 3 * 4])])
        .unwrap();
    engine.load_show(&show(reel)).unwrap();
    engine.advance_frame(0.0);
    engine
}

const ROLL: &str = r#"{ "font": "cell", "duration": 0.1, "direction": "forward" }"#;

/// Per cell: the x of its window, and the y of every character in it.
fn cells(engine: &Engine) -> Vec<(f64, Vec<f64>)> {
    let mut out: Vec<(f64, Vec<f64>)> = Vec::new();
    for layer in engine.resolved_layers().unwrap() {
        match layer.shape {
            ResolvedShape::ClipBegin { ref shape } => {
                let ResolvedShape::Rect { x, .. } = **shape else {
                    panic!("a cell clips to a rect")
                };
                out.push((x, Vec::new()));
            }
            ResolvedShape::Bitmap { y, .. } => {
                out.last_mut()
                    .expect("a character sits in a cell")
                    .1
                    .push(y);
            }
            _ => {}
        }
    }
    out
}

#[test]
fn a_cell_is_a_window_on_two_characters() {
    let engine = engine(ROLL);
    let cells = cells(&engine);
    // "0" is right justified in three cells, so only the last one shows.
    assert_eq!(cells.len(), 1);
    let (x, characters) = &cells[0];
    assert_eq!(*x, 20.0, "the third cell of a 30 wide row");
    // The character it stands on, and the one below it, a cell apart.
    assert_eq!(characters.len(), 2);
    assert!((characters[1] - characters[0] - 10.0).abs() < 1e-9);
}

#[test]
fn a_cell_rolls_to_its_character_and_stops_there() {
    let mut engine = engine(ROLL);
    let slide = |e: &Engine| cells(e)[0].1[0];
    let at_rest = slide(&engine);
    engine.set_variable("score", "1");
    engine.advance_frame(0.05);
    // Halfway through a step the character has slid half a cell up.
    assert!(
        (slide(&engine) - (at_rest - 5.0)).abs() < 1e-9,
        "{}",
        slide(&engine)
    );
    engine.advance_frame(0.05);
    assert!((slide(&engine) - at_rest).abs() < 1e-9);
    // And it stays put.
    engine.advance_frame(1.0);
    assert!((slide(&engine) - at_rest).abs() < 1e-9);
}

#[test]
fn only_the_cells_that_change_move() {
    let mut engine = engine(ROLL);
    engine.set_variable("score", "109");
    // Long enough for the units cell to step all the way from 0 to 9.
    engine.advance_frame(1.0);
    let resting: Vec<f64> = cells(&engine).iter().map(|c| c.1[0]).collect();
    assert_eq!(resting.len(), 3);
    // 109 to 119: only the middle cell moves.
    engine.set_variable("score", "119");
    engine.advance_frame(0.05);
    let moving: Vec<f64> = cells(&engine).iter().map(|c| c.1[0]).collect();
    assert_eq!(moving[0], resting[0]);
    assert!(moving[1] < resting[1], "the tens cell rolls");
    assert_eq!(moving[2], resting[2]);
}

#[test]
fn stagger_lets_the_cells_follow_each_other() {
    let mut engine =
        engine(r#"{ "font": "cell", "duration": 0.1, "direction": "forward", "stagger": 0.05 }"#);
    engine.set_variable("score", "999");
    engine.advance_frame(2.0);
    let resting: Vec<f64> = cells(&engine).iter().map(|c| c.1[0]).collect();
    engine.set_variable("score", "000");
    engine.advance_frame(0.05);
    let moving: Vec<f64> = cells(&engine).iter().map(|c| c.1[0]).collect();
    // The rightmost cell is halfway; the ones to its left still wait.
    assert!(moving[2] < resting[2], "the last cell leads");
    assert_eq!(moving[1], resting[1], "the middle cell waits its turn");
    assert_eq!(moving[0], resting[0]);
}

#[test]
fn a_character_off_the_ring_shows_nothing() {
    let mut engine = engine(ROLL);
    engine.set_variable("score", "A");
    engine.advance_frame(0.0);
    // "A" is in the font but not on the ring of digits.
    assert!(cells(&engine).is_empty());
}

#[test]
fn the_ring_is_the_charset_and_wraps() {
    let mut engine = engine(r#"{ "font": "cell", "charset": "01", "duration": 0.1 }"#);
    let at_rest = cells(&engine)[0].1[0];
    engine.set_variable("score", "1");
    engine.advance_frame(0.1);
    assert!((cells(&engine)[0].1[0] - at_rest).abs() < 1e-9);
    // Back to 0: a two character ring rolls on round, not back.
    engine.set_variable("score", "0");
    engine.advance_frame(0.05);
    assert!(cells(&engine)[0].1[0] < at_rest);
}

#[test]
fn rejects_bad_reels() {
    let bad = |reel: &str, expect: &str| {
        let err = Engine::new()
            .load_show(&show(reel))
            .unwrap_err()
            .to_string();
        assert!(err.contains(expect), "{err}");
    };
    bad(r#"{ "font": "nope", "duration": 0.1 }"#, "undeclared font");
    bad(r#"{ "font": "cell", "duration": 0 }"#, "duration above 0");
    bad(
        r#"{ "font": "cell", "charset": "", "duration": 0.1 }"#,
        "charset",
    );
    bad(
        r#"{ "font": "cell", "duration": 0.1, "stagger": -1 }"#,
        "stagger",
    );
    bad(
        r#"{ "font": "cell", "duration": 0.1, "offset": [ { "t": 0, "v": 1 } ] }"#,
        "starts and ends at 0",
    );
}
