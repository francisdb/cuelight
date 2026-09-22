//! Binding extras: a bindable visible, thresholds and debounce.

use cuelight::{Engine, ResolvedShape};

fn show(layers: &str) -> String {
    format!(
        r#"{{ "name": "bindings", "size": [64, 32], "variables": {{ "lamp": 0, "level": 0.2 }},
             "layers": [{layers}] }}"#
    )
}

fn engine(layers: &str) -> Engine {
    let mut engine = Engine::new();
    engine.load_show(&show(layers)).unwrap();
    engine
}

fn names(engine: &Engine) -> Vec<String> {
    engine
        .resolved_layers()
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect()
}

const BOX: &str = r##""type": "shape", "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF""##;

#[test]
fn visible_follows_a_variable() {
    let mut engine = engine(&format!(
        r#"{{ "name": "bulb", {BOX}, "bindings": [ {{ "property": "visible", "variable": "lamp" }} ] }}"#
    ));
    assert!(names(&engine).is_empty());
    engine.set_variable("lamp", 1.0);
    assert_eq!(names(&engine), ["bulb"]);
    engine.set_variable("lamp", true);
    assert_eq!(names(&engine), ["bulb"]);
    engine.set_variable("lamp", false);
    assert!(names(&engine).is_empty());
}

#[test]
fn visible_hides_a_subtree_and_its_sounds() {
    let mut engine = Engine::new();
    engine.set_sound("hum", 1.0).unwrap();
    engine
        .load_show(&show(&format!(
            r#"{{ "name": "panel", "type": "group", "visible": true,
                  "bindings": [ {{ "property": "visible", "variable": "lamp", "map": {{ "2": true }}, "default": false }} ],
                  "children": [ {{ "name": "box", {BOX} }},
                                {{ "name": "hum", "type": "audio", "sound": "hum", "autoplay": true, "loop": true }} ] }}"#
        )))
        .unwrap();
    assert!(names(&engine).is_empty());
    assert!(engine.voices().unwrap().is_empty());
    engine.set_variable("lamp", 2.0);
    assert_eq!(names(&engine), ["box"]);
    assert_eq!(engine.voices().unwrap().len(), 1);
}

#[test]
fn threshold_turns_a_level_into_on_or_off() {
    let mut engine = engine(&format!(
        r#"{{ "name": "bulb", {BOX},
             "bindings": [ {{ "property": "opacity", "variable": "level", "threshold": 0.5, "scale": 0.8, "offset": 0.2 }} ] }}"#
    ));
    let opacity = |e: &Engine| e.resolved_layers().unwrap()[0].opacity;
    assert!((opacity(&engine) - 0.2).abs() < 1e-9);
    engine.set_variable("level", 0.5);
    assert!((opacity(&engine) - 1.0).abs() < 1e-9);
    engine.set_variable("level", 0.49);
    assert!((opacity(&engine) - 0.2).abs() < 1e-9);
}

#[test]
fn threshold_with_a_transition_gives_a_warm_up() {
    let mut engine = engine(&format!(
        r#"{{ "name": "bulb", {BOX},
             "bindings": [ {{ "property": "opacity", "variable": "level", "threshold": 0.5,
                              "transition": {{ "duration": 1.0 }} }} ] }}"#
    ));
    let opacity = |e: &Engine| e.resolved_layers().unwrap()[0].opacity;
    engine.advance_frame(0.0);
    engine.set_variable("level", 0.9);
    engine.advance_frame(0.5);
    assert!((opacity(&engine) - 0.5).abs() < 1e-9);
}

#[test]
fn debounce_ignores_short_changes() {
    let mut engine = engine(&format!(
        r#"{{ "name": "bulb", {BOX},
             "bindings": [ {{ "property": "x", "variable": "lamp", "scale": 10, "debounce": 0.1 }} ] }}"#
    ));
    let x = |e: &Engine| match e.resolved_layers().unwrap()[0].shape {
        ResolvedShape::Rect { x, .. } => x,
        _ => unreachable!(),
    };
    // The value at load applies at once.
    engine.advance_frame(0.0);
    assert_eq!(x(&engine), 0.0);
    // A strobe: on for 50 ms, then off again. Never shows.
    engine.set_variable("lamp", 1.0);
    engine.advance_frame(0.05);
    assert_eq!(x(&engine), 0.0);
    engine.set_variable("lamp", 0.0);
    engine.advance_frame(0.05);
    engine.advance_frame(0.05);
    assert_eq!(x(&engine), 0.0);
    // Held for the debounce time: shows.
    engine.set_variable("lamp", 1.0);
    engine.advance_frame(0.05);
    assert_eq!(x(&engine), 0.0);
    engine.advance_frame(0.05);
    assert_eq!(x(&engine), 10.0);
    // Back to off, held.
    engine.set_variable("lamp", 0.0);
    engine.advance_frame(0.1);
    assert_eq!(x(&engine), 0.0);
}

#[test]
fn debounce_feeds_the_transition() {
    let mut engine = engine(&format!(
        r#"{{ "name": "bulb", {BOX},
             "bindings": [ {{ "property": "x", "variable": "lamp", "scale": 10, "debounce": 0.1,
                              "transition": {{ "duration": 1.0 }} }} ] }}"#
    ));
    let x = |e: &Engine| match e.resolved_layers().unwrap()[0].shape {
        ResolvedShape::Rect { x, .. } => x,
        _ => unreachable!(),
    };
    engine.advance_frame(0.0);
    engine.set_variable("lamp", 1.0);
    engine.advance_frame(0.1);
    // Settled in this step, and the transition runs from the step's start,
    // as every input between frames does.
    assert!((x(&engine) - 1.0).abs() < 1e-9);
    engine.advance_frame(0.5);
    assert!((x(&engine) - 6.0).abs() < 1e-9);
}

#[test]
fn rejects_bad_binding_extras() {
    let bad = |binding: &str, expect: &str| {
        let err = Engine::new()
            .load_show(&show(&format!(
                r#"{{ "name": "b", {BOX}, "bindings": [ {binding} ] }}"#
            )))
            .unwrap_err()
            .to_string();
        assert!(err.contains(expect), "{err}");
    };
    bad(
        r#"{ "property": "visible", "variable": "lamp", "transition": { "duration": 1 } }"#,
        "cannot be eased",
    );
    bad(
        r#"{ "property": "x", "variable": "lamp", "debounce": -1 }"#,
        "debounce",
    );
    let err = Engine::new()
        .load_show(&show(&format!(
            r#"{{ "name": "b", {BOX}, "timelines": [ {{ "name": "t", "tracks": [ {{ "property": "visible", "keys": [] }} ] }} ] }}"#
        )))
        .unwrap_err()
        .to_string();
    assert!(err.contains("can only be bound"), "{err}");
}
