//! A show that animates a value of its own, read by bindings the way a
//! variable is.

use cuelight::{Engine, ResolvedShape};

/// Two boxes bound to one value, moved in two stretches: the first plays
/// at load and starts the second, which holds its last value.
const CHAIN: &str = r##"{
  "format": 1, "name": "v", "size": [200, 60],
  "values": { "distance": { "timelines": [
    { "name": "approach", "autoplay": true, "on_end": "c1",
      "keys": [{ "t": 0, "v": 0 }, { "t": 2, "v": 40 }] },
    { "name": "partial", "trigger": "c1", "hold": true,
      "keys": [{ "t": 0, "v": 40 }, { "t": 1, "v": 100 }] }
  ] } },
  "layers": [
    { "name": "a", "type": "shape", "shape": { "rect": [0,0,4,4] },
      "fill": "#FFFFFF", "x": 0,
      "bindings": [{ "property": "x", "variable": "distance" }] },
    { "name": "b", "type": "shape", "shape": { "rect": [0,0,4,4] },
      "fill": "#FFFFFF", "x": 0,
      "bindings": [{ "property": "x", "variable": "distance", "scale": 0.5 }] }
  ] }"##;

fn xs(engine: &Engine) -> (f64, f64) {
    let layers = engine.resolved_layers().unwrap();
    let get = |name: &str| match layers.iter().find(|l| l.name == name).unwrap().shape {
        ResolvedShape::Rect { x, .. } => x,
        _ => panic!("{name} should be a rect"),
    };
    (get("a"), get("b"))
}

fn at(to: f64, fps: f64) -> Engine {
    let mut engine = Engine::new();
    engine.load_show(CHAIN).unwrap();
    let step = 1.0 / fps;
    let (mut frame, mut time) = (0u64, 0.0_f64);
    while time < to {
        frame += 1;
        time = (frame as f64 * step).min(to);
        engine.advance_to(time);
        engine.drain_events();
    }
    engine
}

#[test]
fn readers_of_one_value_agree() {
    let (a, b) = xs(&at(1.0, 60.0));
    assert!((a - 20.0).abs() < 1e-9, "a is {a}");
    assert!((b - 10.0).abs() < 1e-9, "b is {b}");
}

#[test]
fn a_value_is_played_in_stretches_chained_by_on_end() {
    // 0..2 s the first, then the second from 40 to 100 over a second,
    // held after.
    for (to, want) in [
        (0.5, 10.0),
        (2.0, 40.0),
        (2.5, 70.0),
        (3.0, 100.0),
        (9.0, 100.0),
    ] {
        let (a, _) = xs(&at(to, 60.0));
        assert!(
            (a - want).abs() < 1e-9,
            "at {to} s the value is {a}, wanted {want}"
        );
    }
}

#[test]
fn a_value_is_the_same_at_any_frame_rate() {
    for to in [0.5, 2.0, 2.5, 3.0] {
        let want = xs(&at(to, 240.0));
        for fps in [60.0, 30.0, 7.0, 1.0, 0.1] {
            assert_eq!(want, xs(&at(to, fps)), "at {to} s, {fps} fps disagrees");
        }
    }
}

#[test]
fn a_host_variable_takes_the_value_over() {
    let mut engine = at(1.0, 60.0);
    assert_eq!(engine.value("distance"), Some(20.0.into()));
    engine.set_variable("distance", 7.0);
    engine.advance_to(engine.time());
    let (a, b) = xs(&engine);
    assert_eq!((a, b), (7.0, 3.5));
}

#[test]
fn reading_a_value_is_not_reading_an_undeclared_variable() {
    let mut engine = Engine::new();
    engine.load_show(CHAIN).unwrap();
    assert!(
        engine.load_warnings().is_empty(),
        "reading a show value warned: {:?}",
        engine.load_warnings()
    );
}

#[test]
fn a_value_still_warns_where_a_number_is_no_use() {
    // A value is always a number, so binding one to a color without a
    // map is as quiet as a variable that starts at the wrong thing.
    let show = r##"{
      "format": 1, "name": "v", "size": [8, 8],
      "values": { "n": { "timelines": [
        { "name": "go", "autoplay": true, "keys": [{ "t": 0, "v": 1 }] } ] } },
      "layers": [{ "name": "pic", "type": "image", "image": "none",
        "bindings": [{ "property": "tint", "variable": "n" }] }] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let warnings = engine.load_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("which is a number"), "{warnings:?}");
}

#[test]
fn a_mistyped_variable_is_still_reported() {
    let show = r##"{
      "format": 1, "name": "v", "size": [8, 8],
      "values": { "zoom": { "timelines": [
        { "name": "go", "autoplay": true, "keys": [{ "t": 0, "v": 1 }] } ] } },
      "layers": [{ "name": "box", "type": "shape", "shape": { "rect": [0,0,4,4] },
        "fill": "#FFFFFF",
        "bindings": [{ "property": "x", "variable": "zoomm" }] }] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    assert_eq!(
        engine.load_warnings().len(),
        1,
        "{:?}",
        engine.load_warnings()
    );
}
