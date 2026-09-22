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

/// The color an image layer is drawn with.
fn tint_of(engine: &Engine) -> [u8; 4] {
    engine.resolved_layers().unwrap()[0].color
}

#[test]
fn tint_follows_a_variable() {
    let mut engine = Engine::new();
    engine.set_image("lamp", 1, 1, vec![255; 4]).unwrap();
    engine
        .load_show(
            r##"{ "name": "status", "size": [64, 32], "variables": { "state": "ok" },
                  "layers": [ { "name": "light", "type": "image", "image": "lamp",
                                "tint": "#808080",
                                "bindings": [ { "property": "tint", "variable": "state",
                                                "map": { "ok": "#00FF00", "warn": "#FFAA00",
                                                         "bad": "#FF0000" } } ] } ] }"##,
        )
        .unwrap();
    engine.advance_frame(0.0);
    assert_eq!(tint_of(&engine), [0, 255, 0, 255]);
    engine.set_variable("state", "bad");
    engine.advance_frame(0.0);
    assert_eq!(
        tint_of(&engine),
        [255, 0, 0, 255],
        "one layer, three states"
    );

    // A value the map does not cover leaves the layer's own tint.
    engine.set_variable("state", "unknown");
    engine.advance_frame(0.0);
    assert_eq!(tint_of(&engine), [128, 128, 128, 255]);
}

#[test]
fn an_empty_tint_leaves_the_image_alone() {
    let mut engine = Engine::new();
    engine.set_image("lamp", 1, 1, vec![255; 4]).unwrap();
    engine
        .load_show(
            r##"{ "name": "status", "size": [64, 32], "variables": { "state": "off" },
                  "layers": [ { "name": "light", "type": "image", "image": "lamp",
                                "tint": "#FF0000",
                                "bindings": [ { "property": "tint", "variable": "state",
                                                "map": { "off": "", "on": "#FF0000" } } ] } ] }"##,
        )
        .unwrap();
    engine.advance_frame(0.0);
    assert_eq!(tint_of(&engine), [255; 4], "an empty tint is no tint");
}

#[test]
fn a_tint_binding_must_map_to_colors() {
    let e = Engine::new()
        .load_show(
            r##"{ "name": "status", "size": [64, 32], "variables": { "state": "ok" },
                  "layers": [ { "name": "light", "type": "image", "image": "lamp",
                                "bindings": [ { "property": "tint", "variable": "state",
                                                "map": { "ok": "green" } } ] } ] }"##,
        )
        .unwrap_err();
    assert!(format!("{e}").contains("not a color"), "{e}");
}

#[test]
fn a_tint_cannot_be_eased() {
    let e = Engine::new()
        .load_show(
            r##"{ "name": "status", "size": [64, 32], "variables": { "state": "ok" },
                  "layers": [ { "name": "light", "type": "image", "image": "lamp",
                                "bindings": [ { "property": "tint", "variable": "state",
                                                "transition": { "duration": 1 } } ] } ] }"##,
        )
        .unwrap_err();
    assert!(format!("{e}").contains("cannot be eased"), "{e}");
}

#[test]
fn a_binding_on_a_variable_the_show_does_not_declare_is_reported() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(&format!(
            r#"{{ "name": "box", {BOX}, "bindings": [
                 {{ "property": "opacity", "variable": "lmap" }} ] }}"#
        )))
        .unwrap();
    let warnings = engine.load_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("lmap"), "{warnings:?}");
    assert!(warnings[0].contains("does not declare"), "{warnings:?}");

    // The binding is not broken, only quiet: a host may set it later.
    engine.set_variable("lmap", 0.5);
    engine.advance_frame(0.0);
    let drawn = &engine.resolved_layers().unwrap()[0];
    assert!((drawn.opacity - 0.5).abs() < 1e-9);
}

#[test]
fn a_tint_variable_that_does_not_start_as_a_color_is_reported() {
    let mut engine = Engine::new();
    engine.set_image("lamp", 1, 1, vec![255; 4]).unwrap();
    engine
        .load_show(
            r##"{ "name": "s", "size": [64, 32], "variables": { "c": "green" },
                  "layers": [ { "name": "light", "type": "image", "image": "lamp",
                                "bindings": [ { "property": "tint", "variable": "c" } ] } ] }"##,
        )
        .unwrap();
    let warnings = engine.load_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("\"green\""), "{warnings:?}");
    assert!(warnings[0].contains("#RRGGBB"), "{warnings:?}");
}

#[test]
fn a_font_variable_that_does_not_start_as_a_style_is_reported() {
    let mut engine = Engine::new();
    let warnings: Vec<String> = {
        engine
            .load_show(
                r##"{ "name": "s", "size": [64, 32], "variables": { "f": "heavy" },
                      "fonts": { "plain": { "file": "blocks" } },
                      "layers": [ { "name": "score", "type": "text", "text": "0",
                                    "font": "plain",
                                    "bindings": [ { "property": "font", "variable": "f" } ] } ] }"##,
            )
            .unwrap();
        engine.load_warnings().to_vec()
    };
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("font styles"), "{warnings:?}");
}

#[test]
fn a_mapped_binding_is_not_reported_for_its_variables_value() {
    let mut engine = Engine::new();
    engine.set_image("lamp", 1, 1, vec![255; 4]).unwrap();
    engine
        .load_show(
            r##"{ "name": "s", "size": [64, 32], "variables": { "state": "ok" },
                  "layers": [ { "name": "light", "type": "image", "image": "lamp",
                                "bindings": [ { "property": "tint", "variable": "state",
                                                "map": { "ok": "#00FF00" } } ] } ] }"##,
        )
        .unwrap();
    assert!(
        engine.load_warnings().is_empty(),
        "a map's values are checked at load, not its variable: {:?}",
        engine.load_warnings()
    );
}
