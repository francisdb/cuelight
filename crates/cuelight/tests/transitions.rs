//! Binding transitions: bound values ease to a new value instead of
//! jumping, as a function of time only.

use cuelight::{BitmapFont, Direction, Easing, Engine, Key, ResolvedShape, Transition};

/// A show with one rect carrying `binding`.
fn show(binding: &str) -> String {
    format!(
        r##"{{
          "format": 1, "name": "t", "size": [200, 100],
          "variables": {{ "pos": 0, "lamp": "off" }},
          "layers": [
            {{ "name": "box", "type": "shape", "shape": {{ "rect": [0, 0, 10, 10] }},
               "fill": "#ffffff", "bindings": [ {binding} ] }}
          ]
        }}"##
    )
}

fn load(binding: &str) -> Engine {
    let mut engine = Engine::new();
    engine.load_show(&show(binding)).unwrap();
    engine
}

fn the_box(engine: &Engine) -> (f64, f64) {
    let layers = engine.resolved_layers().unwrap();
    let layer = layers.iter().find(|l| l.name == "box").unwrap();
    match layer.shape {
        ResolvedShape::Rect { x, .. } => (x, layer.opacity),
        _ => panic!("box should be a rect"),
    }
}

fn x(engine: &Engine) -> f64 {
    the_box(engine).0
}

const EASED_X: &str =
    r#"{ "property": "x", "variable": "pos", "transition": { "duration": 1.0 } }"#;

#[test]
fn a_change_eases_over_the_duration() {
    let mut engine = load(EASED_X);
    engine.advance_frame(0.1);
    assert_eq!(x(&engine), 0.0);

    engine.set_variable("pos", 100.0);
    engine.advance_frame(0.25);
    assert!((x(&engine) - 25.0).abs() < 1e-9);
    engine.advance_frame(0.5);
    assert!((x(&engine) - 75.0).abs() < 1e-9);
    engine.advance_frame(0.5);
    assert_eq!(x(&engine), 100.0);
}

#[test]
fn a_property_starts_at_its_value() {
    // What the host sets before the first frame is where things start.
    let mut engine = load(EASED_X);
    engine.set_variable("pos", 80.0);
    assert_eq!(x(&engine), 80.0);
    engine.advance_frame(0.1);
    assert_eq!(x(&engine), 80.0);
}

#[test]
fn a_new_target_continues_from_the_value_reached() {
    let mut engine = load(EASED_X);
    engine.advance_frame(0.1);
    engine.set_variable("pos", 100.0);
    engine.advance_frame(0.5);
    engine.set_variable("pos", 0.0);
    // From 50 back to 0 over a full second.
    engine.advance_frame(0.5);
    assert!((x(&engine) - 25.0).abs() < 1e-9);
    engine.advance_frame(0.5);
    assert_eq!(x(&engine), 0.0);
}

#[test]
fn the_value_does_not_depend_on_the_frame_rate() {
    let binding = r#"{ "property": "x", "variable": "pos",
        "transition": { "duration": 1.0, "ease": "cubic_in_out" } }"#;
    let run = |steps: u32| {
        let mut engine = load(binding);
        engine.advance_frame(0.1);
        engine.set_variable("pos", 100.0);
        for _ in 0..steps {
            engine.advance_frame(0.6 / f64::from(steps));
        }
        x(&engine)
    };
    assert!((run(1) - run(36)).abs() < 1e-9);
    assert!((run(2) - run(144)).abs() < 1e-9);
}

#[test]
fn mapped_values_fade() {
    let mut engine = load(
        r#"{ "property": "opacity", "variable": "lamp", "map": { "on": 1 }, "default": 0.2,
             "transition": { "duration": 0.2 } }"#,
    );
    engine.advance_frame(0.1);
    assert!((the_box(&engine).1 - 0.2).abs() < 1e-9);
    engine.set_variable("lamp", "on");
    engine.advance_frame(0.1);
    assert!((the_box(&engine).1 - 0.6).abs() < 1e-9);
    engine.advance_frame(0.1);
    assert!((the_box(&engine).1 - 1.0).abs() < 1e-9);
}

#[test]
fn a_running_timeline_wins_and_hands_back_the_eased_value() {
    let mut engine = Engine::new();
    engine
        .load_show(
            r##"{
              "format": 1, "name": "t", "size": [200, 100],
              "variables": { "pos": 0 },
              "layers": [
                { "name": "box", "type": "shape", "shape": { "rect": [0, 0, 10, 10] },
                  "fill": "#ffffff",
                  "bindings": [ { "property": "x", "variable": "pos",
                                  "transition": { "duration": 1.0 } } ],
                  "timelines": [ { "name": "shake", "trigger": "shake", "tracks": [
                    { "property": "x", "keys": [ { "t": 0, "v": 500 }, { "t": 0.5, "v": 500 } ] }
                  ] } ] }
              ]
            }"##,
        )
        .unwrap();
    engine.advance_frame(0.1);
    engine.set_variable("pos", 100.0);
    engine.trigger("shake");
    engine.advance_frame(0.25);
    assert_eq!(x(&engine), 500.0);
    // The transition kept going underneath.
    engine.advance_frame(0.5);
    assert!((x(&engine) - 75.0).abs() < 1e-9);
}

#[test]
fn entering_a_scene_starts_at_the_value() {
    let mut engine = Engine::new();
    engine
        .load_show(
            r##"{
              "format": 1, "name": "t", "size": [200, 100],
              "variables": { "pos": 0 },
              "scenes": [
                { "name": "idle", "trigger": "idle", "layers": [] },
                { "name": "game", "trigger": "game", "layers": [
                  { "name": "box", "type": "shape", "shape": { "rect": [0, 0, 10, 10] },
                    "fill": "#ffffff",
                    "bindings": [ { "property": "x", "variable": "pos",
                                    "transition": { "duration": 1.0 } } ] } ] }
              ]
            }"##,
        )
        .unwrap();
    engine.advance_frame(0.1);
    engine.set_variable("pos", 100.0);
    engine.advance_frame(0.1);
    engine.trigger("game");
    engine.advance_frame(0.1);
    assert_eq!(x(&engine), 100.0);
    // Seen once, left, changed meanwhile, entered again: no sweep either.
    engine.trigger("idle");
    engine.set_variable("pos", 20.0);
    engine.advance_frame(0.1);
    engine.trigger("game");
    engine.advance_frame(0.1);
    assert_eq!(x(&engine), 20.0);
}

// Every glyph is 2 wide with an advance of 3, so n characters measure
// 3n - 1: the width tells how many characters the score has.
const FNT: &str = r#"info face="Blocks" size=3
common lineHeight=4 base=3 scaleW=2 scaleH=3 pages=1
page id=0 file="blocks_0.png"
char id=32 x=0 y=0 width=0 height=0 xoffset=0 yoffset=0 xadvance=3 page=0
char id=48 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=49 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
char id=53 x=0 y=0 width=2 height=3 xoffset=0 yoffset=0 xadvance=3 page=0
"#;

fn score_width(engine: &Engine) -> f64 {
    let layers = engine.resolved_layers().unwrap();
    match layers.iter().find(|l| l.name == "score").unwrap().shape {
        ResolvedShape::Bitmap { width, .. } => width,
        ref other => panic!("score should be a bitmap, got {other:?}"),
    }
}

#[test]
fn numbers_in_text_count_up_in_whole_numbers() {
    let mut engine = Engine::new();
    let font = BitmapFont::parse(FNT).unwrap();
    engine
        .set_font("blocks", font, vec![(2, 3, vec![255; 2 * 3 * 4])])
        .unwrap();
    engine
        .load_show(
            r##"{
              "format": 1, "name": "t", "size": [64, 8],
              "fonts": { "white": { "file": "blocks" } },
              "variables": { "score": 0 },
              "layers": [
                { "name": "score", "type": "text", "font": "white", "text": "0",
                  "bindings": [ { "property": "text", "variable": "score",
                                  "transition": { "duration": 1.0 } } ] }
              ]
            }"##,
        )
        .unwrap();
    engine.advance_frame(0.1);
    assert_eq!(score_width(&engine), 2.0);

    engine.set_variable("score", 1000.0);
    // 1000 * 0.155 is not exactly 155 in floating point: "155" it is.
    engine.advance_frame(0.155);
    assert_eq!(score_width(&engine), 8.0);
    engine.advance_frame(0.845);
    assert_eq!(score_width(&engine), 11.0);

    // Text that is not a number has nothing to count through.
    engine.set_variable("score", "5 5 5");
    engine.advance_frame(0.01);
    assert_eq!(score_width(&engine), 14.0);
}

#[test]
fn wrapped_values_pick_their_way_round() {
    let t = |direction| Transition {
        duration: 1.0,
        ease: Easing::Linear,
        wrap: Some(360.0),
        direction,
        step: None,
        offset: Vec::new(),
    };
    // The short way from 350 to 10 is forward through 0.
    assert_eq!(t(None).value_at(350.0, 10.0, 0.5), 0.0);
    assert_eq!(t(None).value_at(10.0, 350.0, 0.5), 0.0);
    // Forced ways round.
    assert_eq!(
        t(Some(Direction::Forward)).value_at(10.0, 350.0, 0.5),
        180.0
    );
    assert_eq!(
        t(Some(Direction::Backward)).value_at(350.0, 10.0, 0.5),
        180.0
    );
    // A reel: 9 to 0 rolls on, and lands exactly.
    let reel = Transition {
        duration: 1.0,
        ease: Easing::Linear,
        wrap: Some(10.0),
        direction: Some(Direction::Forward),
        step: None,
        offset: Vec::new(),
    };
    assert!((reel.value_at(9.0, 0.0, 0.5) - 9.5).abs() < 1e-9);
    assert_eq!(reel.value_at(9.0, 0.0, 1.0), 0.0);
    assert_eq!(reel.value_at(9.0, 0.0, 5.0), 0.0);
    // An ease that overshoots: the reel swings past the digit, through the
    // wrap if it has to, and still lands exactly.
    let snap = Transition {
        ease: Easing::BackOut,
        ..reel.clone()
    };
    let peak = snap.value_at(9.0, 0.0, 0.6);
    assert!(peak > 0.0 && peak < 0.2, "past 0 on the far side: {peak}");
    assert_eq!(snap.value_at(9.0, 0.0, 1.0), 0.0);
    // Not moving is not a full turn.
    assert_eq!(t(Some(Direction::Backward)).value_at(90.0, 90.0, 0.5), 90.0);
    assert_eq!(t(Some(Direction::Forward)).value_at(90.0, 450.0, 0.5), 90.0);
}

/// A reel move: there in 0.1 s, then out by 0.12 and back by 0.2 s.
fn kick() -> Transition {
    let key = |t, v| Key {
        t,
        v,
        ease: Easing::Linear,
    };
    Transition {
        duration: 0.1,
        ease: Easing::Linear,
        wrap: None,
        direction: None,
        step: None,
        offset: vec![key(0.1, 0.0), key(0.15, 0.12), key(0.2, 0.0)],
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn an_offset_is_the_same_size_however_far_the_move_goes() {
    let kick = kick();
    assert_eq!(kick.move_time(), 0.2);
    // Swinging past the target by 0.12, for a move of 1 and a move of 9.
    assert!(close(kick.value_at(0.0, 1.0, 0.15), 1.12));
    assert!(close(kick.value_at(0.0, 9.0, 0.15), 9.12));
    // Along the direction of travel: going down it swings below.
    assert!(close(kick.value_at(9.0, 8.0, 0.15), 7.88));
    // Nothing of it before the move is done, nothing left after.
    assert!(close(kick.value_at(0.0, 1.0, 0.05), 0.5));
    assert_eq!(kick.value_at(0.0, 1.0, 0.2), 1.0);
}

#[test]
fn a_step_plays_a_large_change_as_moves_in_a_row() {
    let chug = Transition {
        step: Some(1.0),
        ..kick()
    };
    // Three kicks of 0.2 s each, every one with its settle.
    assert!(close(chug.value_at(0.0, 3.0, 0.05), 0.5));
    assert!(close(chug.value_at(0.0, 3.0, 0.15), 1.12));
    assert!(close(chug.value_at(0.0, 3.0, 0.25), 1.5));
    assert!(close(chug.value_at(0.0, 3.0, 0.55), 3.12));
    assert_eq!(chug.value_at(0.0, 3.0, 0.6), 3.0);
    // Two and a half: the last move is the short one.
    assert!(close(chug.value_at(0.0, 2.5, 0.45), 2.25));
    assert_eq!(chug.value_at(0.0, 2.5, 0.6), 2.5);
    // Smaller than a step is one move, as without it.
    assert!(close(chug.value_at(0.0, 0.5, 0.05), 0.25));
}

#[test]
fn a_stepped_reel_rolls_through_the_wrap_and_settles_each_digit() {
    let reel = Transition {
        step: Some(1.0),
        wrap: Some(10.0),
        direction: Some(Direction::Forward),
        ..kick()
    };
    // 8 to 1 is three kicks: 9, 0, 1.
    assert!(close(reel.value_at(8.0, 1.0, 0.15), 9.12));
    assert!(close(reel.value_at(8.0, 1.0, 0.35), 0.12));
    assert!(close(reel.value_at(8.0, 1.0, 0.25), 9.5));
    assert_eq!(reel.value_at(8.0, 1.0, 0.6), 1.0);
}

#[test]
fn step_and_offset_load_from_a_show() {
    let mut engine = load(
        r#"{ "property": "x", "variable": "pos",
             "transition": { "duration": 0.1, "step": 10,
               "offset": [ { "t": 0.1, "v": 0 }, { "t": 0.15, "v": 3 }, { "t": 0.2, "v": 0 } ] } }"#,
    );
    engine.advance_frame(0.1);
    engine.set_variable("pos", 20.0);
    engine.advance_frame(0.15);
    assert!(close(x(&engine), 13.0));
    engine.advance_frame(0.2);
    assert!(close(x(&engine), 23.0));
    engine.advance_frame(0.05);
    assert_eq!(x(&engine), 20.0);
}

#[test]
fn bad_transitions_are_refused() {
    let refused = |binding: &str| {
        let mut engine = Engine::new();
        engine.load_show(&show(binding)).unwrap_err().to_string()
    };
    let zero =
        refused(r#"{ "property": "x", "variable": "pos", "transition": { "duration": 0 } }"#);
    assert!(zero.contains("duration above 0"), "{zero}");
    let wrap = refused(
        r#"{ "property": "x", "variable": "pos", "transition": { "duration": 1, "wrap": -1 } }"#,
    );
    assert!(wrap.contains("wrap above 0"), "{wrap}");
    let direction = refused(
        r#"{ "property": "x", "variable": "pos",
             "transition": { "duration": 1, "direction": "forward" } }"#,
    );
    assert!(direction.contains("needs wrap"), "{direction}");
    let step = refused(
        r#"{ "property": "x", "variable": "pos", "transition": { "duration": 1, "step": 0 } }"#,
    );
    assert!(step.contains("step above 0"), "{step}");
    let resting = refused(
        r#"{ "property": "x", "variable": "pos", "transition": { "duration": 1,
             "offset": [ { "t": 0, "v": 0 }, { "t": 1, "v": 2 } ] } }"#,
    );
    assert!(resting.contains("starts and ends at 0"), "{resting}");
    let order = refused(
        r#"{ "property": "x", "variable": "pos", "transition": { "duration": 1,
             "offset": [ { "t": 1, "v": 0 }, { "t": 0.5, "v": 0 } ] } }"#,
    );
    assert!(order.contains("time order"), "{order}");
}
