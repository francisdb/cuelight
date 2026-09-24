//! What the model owes: state at an instant is a function of the show,
//! its input and that instant, and of nothing else.
//!
//! Every test here asks for the same instant at wildly different frame
//! rates and requires the same answer, down to the last bit. The rates
//! include ones coarser than the show's own detail, where a frame spans
//! several of its events, and 0.1 fps, where reaching the instant takes
//! a single clipped step. If any of them disagree, something in the
//! engine is counting frames instead of deriving from time.
//!
//! State is read with [`Engine::values`], one level below the draw list:
//! what the timing model produced, before any of it becomes geometry.
//!
//! [`COVERED`] at the foot of this file lists what is mapped and what
//! is deliberately not.

use cuelight::{Engine, Property, Value};

/// Something the host does, at the instant it does it.
enum Input {
    Trigger(&'static str),
    Set(&'static str, f64),
    SetText(&'static str, &'static str),
}

/// The frame rates every case is checked at. 0.1 fps reaches most
/// instants in one clipped step; 240 splits each of the show's events
/// across several frames.
const RATES: [f64; 7] = [240.0, 60.0, 50.0, 30.0, 7.0, 1.0, 0.1];

/// Play `show` to exactly `to`, stepping at `fps` and applying `script`
/// at the instants it names.
///
/// A step never runs past the next thing that has to happen: the next
/// frame boundary, the next input, or the target. So an input at 0.37 s
/// happens at 0.37 s whether frames are 4 ms or 10 s apart, and the
/// sampling rate cannot leak into the answer.
///
/// The clock is told the instant to land on rather than a delta, because
/// a delta is `previous + how much` and that is not the instant meant.
fn state(show: &str, script: &[(f64, Input)], to: f64, fps: f64) -> Vec<(String, Property, Value)> {
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let step = 1.0 / fps;
    let (mut time, mut frame, mut next) = (0.0_f64, 0_u64, 0_usize);
    while time < to {
        while let Some((at, input)) = script.get(next).filter(|(at, _)| *at <= time) {
            match input {
                Input::Trigger(name) => engine.trigger(name),
                Input::Set(name, v) => engine.set_variable(name, *v),
                Input::SetText(name, v) => engine.set_variable(name, *v),
            }
            let _ = at;
            next += 1;
        }
        let boundary = (frame + 1) as f64 * step;
        let input_at = script.get(next).map_or(f64::MAX, |(at, _)| *at);
        let landing = boundary.min(input_at).min(to);
        engine.advance_to(landing);
        engine.drain_events();
        if landing >= boundary {
            frame += 1;
        }
        time = landing;
    }
    while let Some((_, input)) = script.get(next).filter(|(at, _)| *at <= to) {
        match input {
            Input::Trigger(name) => engine.trigger(name),
            Input::Set(name, v) => engine.set_variable(name, *v),
            Input::SetText(name, v) => engine.set_variable(name, *v),
        }
        next += 1;
    }
    engine.values().unwrap()
}

/// Require the same state at each instant however fast it is sampled.
#[track_caller]
fn same_at_any_rate(what: &str, show: &str, script: &[(f64, Input)], instants: &[f64]) {
    let mut seen: Vec<Vec<(String, Property, Value)>> = Vec::new();
    for &to in instants {
        let want = state(show, script, to, RATES[0]);
        assert!(!want.is_empty(), "{what}: no state at {to} s");
        seen.push(want.clone());
        for &fps in &RATES[1..] {
            let got = state(show, script, to, fps);
            for (a, b) in want.iter().zip(&got) {
                assert!(
                    a == b,
                    "{what}: at {to} s, {} fps says {}.{:?} = {:?}, {fps} fps says {:?}",
                    RATES[0],
                    a.0,
                    a.1,
                    a.2,
                    b.2
                );
            }
            assert_eq!(
                want.len(),
                got.len(),
                "{what}: at {to} s the state differs in size"
            );
        }
    }
    // A case where nothing ever moves would agree at every frame rate
    // and prove nothing. Every case has to exercise what it names.
    assert!(
        seen.windows(2).any(|w| w[0] != w[1]),
        "{what}: the state never changes across {instants:?}, so this case tests nothing"
    );
}

/// A one-layer show wrapped around whatever the case needs.
fn show(layer: &str) -> String {
    format!(r##"{{ "format": 1, "name": "t", "size": [100, 100], "layers": [{layer}] }}"##)
}

// --- Timelines ------------------------------------------------------

#[test]
fn timeline_keys_and_easings() {
    // Every easing, including the three that do not go straight there.
    for ease in [
        "linear",
        "quad_in",
        "quad_out",
        "quad_in_out",
        "cubic_in",
        "cubic_out",
        "cubic_in_out",
        "step",
        "back_in",
        "back_out",
        "back_in_out",
        "elastic_out",
        "bounce_out",
    ] {
        let s = show(&format!(
            r##"{{ "name": "box", "type": "shape", "shape": {{ "rect": [0,0,4,4] }},
                   "fill": "#FFFFFF", "x": 0,
                   "timelines": [{{ "name": "go", "autoplay": true, "hold": true,
                     "tracks": [{{ "property": "x", "keys": [
                       {{ "t": 0, "v": 0 }}, {{ "t": 0.7, "v": 50, "ease": "{ease}" }} ]}}] }}] }}"##
        ));
        same_at_any_rate(ease, &s, &[], &[0.0, 0.17, 0.35, 0.6999, 0.7, 0.9]);
    }
}

#[test]
fn timeline_delay_repeat_loop_and_hold() {
    for (what, extra, instants) in [
        (
            "delay",
            r#""delay": 0.33,"#,
            vec![0.0, 0.32, 0.33, 0.5, 1.03, 1.2],
        ),
        (
            "repeat",
            r#""repeat": 2.5,"#,
            vec![0.3, 0.7, 1.0, 1.4, 1.75, 1.8],
        ),
        ("loop", r#""loop": true,"#, vec![0.3, 0.7, 1.4, 2.1, 5.35]),
        ("hold", r#""hold": true,"#, vec![0.3, 0.7, 0.71, 3.0]),
    ] {
        let s = show(&format!(
            r##"{{ "name": "box", "type": "shape", "shape": {{ "rect": [0,0,4,4] }},
                   "fill": "#FFFFFF", "x": 0,
                   "timelines": [{{ "name": "go", "autoplay": true, {extra}
                     "tracks": [{{ "property": "x", "keys": [
                       {{ "t": 0, "v": 0 }}, {{ "t": 0.7, "v": 50 }} ]}}] }}] }}"##
        ));
        same_at_any_rate(what, &s, &[], &instants);
    }
}

#[test]
fn timeline_chain_by_on_end() {
    // Four links of 0.7 s: the instants straddle every handover.
    let s = show(
        r##"{ "name": "box", "type": "shape", "shape": { "rect": [0,0,4,4] },
              "fill": "#FFFFFF", "x": 0,
              "timelines": [
        { "name": "a", "trigger": "go", "on_end": "d1", "hold": true,
          "tracks": [{ "property": "x", "keys": [{"t":0,"v":0},{"t":0.7,"v":10}] }] },
        { "name": "b", "trigger": "d1", "on_end": "d2", "hold": true,
          "tracks": [{ "property": "y", "keys": [{"t":0,"v":0},{"t":0.7,"v":10}] }] },
        { "name": "c", "trigger": "d2", "on_end": "d3", "hold": true,
          "tracks": [{ "property": "opacity", "keys": [{"t":0,"v":0},{"t":0.7,"v":1}] }] },
        { "name": "d", "trigger": "d3", "hold": true,
          "tracks": [{ "property": "rotation", "keys": [{"t":0,"v":0},{"t":0.7,"v":90}] }] }
      ] }"##,
    );
    let inputs = [(0.0, Input::Trigger("go"))];
    same_at_any_rate(
        "on_end chain",
        &s,
        &inputs,
        &[0.35, 0.7, 0.701, 1.05, 1.4, 1.75, 2.1, 2.45, 2.8, 2.81, 4.0],
    );
}

#[test]
fn timeline_started_part_way_through_a_frame() {
    // Triggers at instants no frame rate here lands on.
    let s = show(
        r##"{ "name": "box", "type": "shape", "shape": { "rect": [0,0,4,4] },
              "fill": "#FFFFFF", "x": 0,
              "timelines": [{ "name": "go", "trigger": "go",
                "tracks": [{ "property": "x", "keys": [{"t":0,"v":0},{"t":1,"v":50}] }] }] }"##,
    );
    let inputs = [(0.37, Input::Trigger("go")), (0.911, Input::Trigger("go"))];
    same_at_any_rate(
        "mid-frame start",
        &s,
        &inputs,
        &[0.4, 0.9, 0.95, 1.5, 1.911, 2.0],
    );
}

#[test]
fn timeline_chain_whose_links_straddle_a_frame() {
    // Links half a microsecond longer than a 60 fps frame, so at that
    // rate every end lands just past a boundary and at 240 it does not.
    // A handover timed from the clock rather than from the instant it
    // should have happened rounds itself onto the boundary and the chain
    // runs fast; a handover that leaves nobody owning the property for a
    // frame shows up here too.
    let s = show(
        r##"{ "name": "box", "type": "shape", "shape": { "rect": [0,0,4,4] },
              "fill": "#FFFFFF", "x": -1,
              "timelines": [
        { "name": "a", "trigger": ["go", "d2"], "on_end": "d1",
          "tracks": [{ "property": "x",
                       "keys": [{"t":0,"v":0},{"t":0.0166671666666667,"v":10}] }] },
        { "name": "b", "trigger": "d1", "on_end": "d2",
          "tracks": [{ "property": "x",
                       "keys": [{"t":0,"v":10},{"t":0.0166671666666667,"v":20}] }] }
      ] }"##,
    );
    let inputs = [(0.0, Input::Trigger("go"))];
    same_at_any_rate("straddling chain", &s, &inputs, &[0.5, 1.0, 2.0, 5.0, 9.0]);
}

// --- Bindings and variables over time -------------------------------

/// A show whose `x`, `opacity`, `visible` and `tint` follow variables,
/// each binding wired a different way.
fn bound(extra: &str) -> String {
    format!(
        r##"{{ "format": 1, "name": "t", "size": [100, 100],
               "variables": {{ "n": 0, "state": "ok" }},
               "layers": [{{ "name": "box", "type": "shape",
                 "shape": {{ "rect": [0,0,4,4] }}, "fill": "#FFFFFF",
                 "x": 0, "opacity": 1, "bindings": [{extra}] }}] }}"##
    )
}

/// Variables moved at instants no frame rate here lands on.
const WIGGLE: [(f64, Input); 6] = [
    (0.13, Input::Set("n", 40.0)),
    (0.4567, Input::Set("n", 5.0)),
    (0.71, Input::Set("n", 90.0)),
    (1.2345, Input::Set("n", 0.0)),
    (1.9, Input::Set("n", 62.0)),
    (2.66, Input::Set("n", 12.0)),
];

#[test]
fn binding_plain_scaled_and_offset() {
    let s = bound(r#"{ "property": "x", "variable": "n", "scale": 0.5, "offset": 3 }"#);
    same_at_any_rate(
        "scale/offset",
        &s,
        &WIGGLE,
        &[0.1, 0.13, 0.5, 1.0, 1.9, 2.8, 3.5],
    );
}

#[test]
fn binding_threshold() {
    let s = bound(r#"{ "property": "visible", "variable": "n", "threshold": 50 }"#);
    same_at_any_rate(
        "threshold",
        &s,
        &WIGGLE,
        &[0.1, 0.13, 0.5, 0.71, 1.5, 2.0, 3.0],
    );
}

#[test]
fn binding_transition() {
    // A ramp interrupted part way through has to carry on from where it
    // was, which is the anchor most likely to be got wrong.
    for ease in ["linear", "cubic_out", "quad_in"] {
        let s = bound(&format!(
            r#"{{ "property": "x", "variable": "n",
                  "transition": {{ "duration": 0.4, "ease": "{ease}" }} }}"#
        ));
        same_at_any_rate(
            ease,
            &s,
            &WIGGLE,
            &[0.13, 0.3, 0.4567, 0.5, 0.6, 0.71, 0.9, 1.3, 2.0, 2.9, 4.0],
        );
    }
}

#[test]
fn binding_transition_of_a_color() {
    let s = r##"{ "format": 1, "name": "t", "size": [100, 100],
               "variables": { "state": "ok" },
               "layers": [{ "name": "pic", "type": "image", "image": "none",
                 "bindings": [{ "property": "tint", "variable": "state",
                   "map": { "ok": "#00FF00", "warn": "#FFAA00", "bad": "#FF0000" },
                   "transition": { "duration": 0.3 } }] }] }"##;
    let script = [
        (0.21, Input::SetText("state", "warn")),
        (0.55, Input::SetText("state", "bad")),
        (1.4, Input::SetText("state", "ok")),
    ];
    same_at_any_rate(
        "tint transition",
        s,
        &script,
        &[0.1, 0.21, 0.35, 0.51, 0.55, 0.7, 0.9, 1.5, 1.8, 2.0],
    );
}

#[test]
fn binding_debounce() {
    // A candidate that has not held long enough must not settle, and the
    // instant it does settle is `since + hold`, not a frame boundary.
    let s = bound(
        r#"{ "property": "visible", "variable": "n",
                       "threshold": 50, "debounce": 0.2 }"#,
    );
    same_at_any_rate(
        "debounce",
        &s,
        &WIGGLE,
        &[0.13, 0.25, 0.33, 0.4567, 0.71, 0.91, 1.0, 1.4345, 2.1, 3.0],
    );
}

// --- Media ----------------------------------------------------------

/// Sound and video are read through `voices()` and `videos()` rather
/// than `values()`: a play's position is state the layer tree does not
/// carry. Both are compared here the same way.
fn playing(show: &str, script: &[(f64, Input)], to: f64, fps: f64) -> Vec<String> {
    let mut engine = Engine::new();
    for name in ["a", "b", "c"] {
        engine.set_sound(name, 0.7).unwrap();
        engine.set_video(name, 0.7, [8.0, 8.0]).unwrap();
    }
    engine.load_show(show).unwrap();
    let step = 1.0 / fps;
    let (mut time, mut frame, mut next) = (0.0_f64, 0_u64, 0_usize);
    while time < to {
        while let Some((_, input)) = script.get(next).filter(|(at, _)| *at <= time) {
            match input {
                Input::Trigger(name) => engine.trigger(name),
                Input::Set(name, v) => engine.set_variable(name, *v),
                Input::SetText(name, v) => engine.set_variable(name, *v),
            }
            next += 1;
        }
        let boundary = (frame + 1) as f64 * step;
        let input_at = script.get(next).map_or(f64::MAX, |(at, _)| *at);
        let landing = boundary.min(input_at).min(to);
        engine.advance_to(landing);
        engine.drain_events();
        if landing >= boundary {
            frame += 1;
        }
        time = landing;
    }
    let mut out: Vec<String> = engine
        .voices()
        .unwrap()
        .iter()
        .map(|v| {
            format!(
                "voice {} {} {:.9} {:.6}",
                v.layer, v.sound, v.position, v.gain
            )
        })
        .collect();
    out.extend(
        engine
            .videos()
            .unwrap()
            .iter()
            .map(|v| format!("video {} {} {:.9}", v.layer, v.video, v.position)),
    );
    out.sort();
    out
}

#[track_caller]
fn plays_the_same_at_any_rate(what: &str, show: &str, script: &[(f64, Input)], instants: &[f64]) {
    let mut seen = Vec::new();
    for &to in instants {
        let want = playing(show, script, to, RATES[0]);
        seen.push(want.clone());
        for &fps in &RATES[1..] {
            assert_eq!(
                want,
                playing(show, script, to, fps),
                "{what}: at {to} s, {} fps and {fps} fps disagree",
                RATES[0]
            );
        }
    }
    assert!(
        seen.windows(2).any(|w| w[0] != w[1]),
        "{what}: nothing ever plays or stops, so this case tests nothing"
    );
}

#[test]
fn media_position_delay_and_repeat() {
    for (what, extra) in [
        ("plain", ""),
        ("delay", r#""delay": 0.23,"#),
        ("repeat", r#""repeat": 2.5,"#),
        ("loop", r#""loop": true,"#),
    ] {
        let s = show(&format!(
            r##"{{ "name": "spk", "type": "audio", "sound": "a", {extra} "trigger": "go" }}"##
        ));
        let script = [(0.17, Input::Trigger("go"))];
        plays_the_same_at_any_rate(
            what,
            &s,
            &script,
            &[0.1, 0.17, 0.3, 0.4, 0.87, 0.88, 1.0, 1.5, 2.0],
        );
    }
}

#[test]
fn media_retrigger_and_voices() {
    for retrigger in ["restart", "ignore", "queue", "overlap"] {
        let s = show(&format!(
            r##"{{ "name": "spk", "type": "audio", "sound": "a",
                   "trigger": "go", "retrigger": "{retrigger}", "voices": 2 }}"##
        ));
        let script = [
            (0.13, Input::Trigger("go")),
            (0.31, Input::Trigger("go")),
            (0.5, Input::Trigger("go")),
            (1.111, Input::Trigger("go")),
        ];
        plays_the_same_at_any_rate(
            retrigger,
            &s,
            &script,
            &[0.2, 0.4, 0.6, 0.83, 0.9, 1.2, 1.5, 2.2, 3.0],
        );
    }
}

#[test]
fn media_chained_by_on_end() {
    let s = show(
        r##"{ "name": "screen", "type": "video", "video": "a",
              "trigger": "go", "on_end": "next" }"##,
    );
    let s = s.replace(
        r#""layers": ["#,
        r#""layers": [{ "name": "screen2", "type": "video", "video": "b",
                        "trigger": "next", "on_end": "again" },"#,
    );
    let script = [(0.11, Input::Trigger("go"))];
    plays_the_same_at_any_rate(
        "media chain",
        &s,
        &script,
        &[0.2, 0.5, 0.81, 0.9, 1.2, 1.51, 1.6, 2.0],
    );
}

#[test]
fn media_rest_and_pick() {
    // `rest` drops a trigger too soon after the last play, and which of
    // several clips a play takes comes from the play count and the seed.
    let s = show(
        r##"{ "name": "spk", "type": "audio", "sound": ["a", "b", "c"],
              "trigger": "go", "pick": "shuffle", "rest": 0.35 }"##,
    );
    let script = [
        (0.1, Input::Trigger("go")),
        (0.3, Input::Trigger("go")),
        (0.52, Input::Trigger("go")),
        (0.9, Input::Trigger("go")),
        (1.7, Input::Trigger("go")),
    ];
    plays_the_same_at_any_rate(
        "rest and pick",
        &s,
        &script,
        &[0.2, 0.4, 0.6, 1.0, 1.4, 2.0, 2.6],
    );
}

// --- Ducking, reels, counters and scenes ----------------------------

#[test]
fn ducking_under_a_bus() {
    // A bed whose level ramps down while another bus sounds and back up
    // after: the ramp is anchored where it was interrupted, so a second
    // clip landing mid-release is the case to get wrong.
    let s = r##"{ "format": 1, "name": "t", "size": [100, 100], "layers": [
          { "name": "bed", "type": "audio", "sound": "a", "loop": true,
             "autoplay": true, "bus": "music",
             "duck": { "under": "voice", "to": 0.1, "attack": 0.15, "release": 0.4 } },
          { "name": "vox", "type": "audio", "sound": "b", "bus": "voice",
             "trigger": "say", "retrigger": "restart" } ] }"##;
    let script = [
        (0.23, Input::Trigger("say")),
        (1.17, Input::Trigger("say")),
        (1.55, Input::Trigger("say")),
    ];
    plays_the_same_at_any_rate(
        "duck",
        s,
        &script,
        &[
            0.1, 0.23, 0.3, 0.5, 0.93, 1.0, 1.2, 1.4, 1.55, 1.9, 2.3, 3.0,
        ],
    );
}

#[test]
fn reel_cells_spinning() {
    // Each cell is anchored where it stood when it was told to move, and
    // the cells are staggered, so several are mid-flight at once.
    let s = r##"{ "format": 1, "name": "t", "size": [200, 60],
               "variables": { "score": 0 },
               "layers": [{ "name": "sc", "type": "digits", "digits": 6,
                 "size": [180, 48], "justify": "right",
                 "display": { "reel": { "charset": "0123456789",
                   "cells": { "images": ["c0","c1","c2","c3","c4",
                                          "c5","c6","c7","c8","c9"] },
                   "duration": 0.12, "ease": "quad_in", "stagger": 0.03 } },
                 "bindings": [{ "property": "text", "variable": "score" }] }] }"##;
    let script = [
        (0.11, Input::Set("score", 4.0)),
        (0.19, Input::Set("score", 1234.0)),
        (0.4567, Input::Set("score", 999999.0)),
        (1.3, Input::Set("score", 7.0)),
    ];
    same_at_any_rate(
        "reel",
        s,
        &script,
        &[0.11, 0.15, 0.2, 0.25, 0.31, 0.5, 0.6, 0.8, 1.35, 1.5, 2.0],
    );
}

#[test]
fn counter_stepping_through_whole_numbers() {
    let s = r##"{ "format": 1, "name": "t", "size": [200, 60],
               "variables": { "score": 0 },
               "fonts": { "plain": { "file": "none" } },
               "layers": [{ "name": "sc", "type": "text", "text": "0",
                 "font": "plain", "bindings": [{ "property": "text", "variable": "score",
                   "transition": { "duration": 0.5, "step": 1 } }] }] }"##;
    let script = [
        (0.13, Input::Set("score", 100.0)),
        (0.77, Input::Set("score", 20.0)),
    ];
    same_at_any_rate(
        "counter",
        s,
        &script,
        &[0.13, 0.2, 0.31, 0.45, 0.63, 0.77, 0.9, 1.1, 1.4],
    );
}

#[test]
fn scenes_entered_part_way_through_a_frame() {
    let s = r##"{ "format": 1, "name": "t", "size": [100, 100],
      "layers": [{ "name": "always", "type": "shape",
                   "shape": { "rect": [0,0,4,4] }, "fill": "#FFFFFF", "x": 1 }],
      "scenes": [
        { "name": "one", "trigger": "one", "layers": [
          { "name": "a", "type": "shape", "shape": { "rect": [0,0,4,4] },
            "fill": "#FFFFFF", "x": 0,
            "timelines": [{ "name": "in", "autoplay": true, "hold": true,
              "tracks": [{ "property": "x", "keys": [{"t":0,"v":0},{"t":0.6,"v":40}] }] }] }] },
        { "name": "two", "trigger": "two", "layers": [
          { "name": "b", "type": "shape", "shape": { "rect": [0,0,4,4] },
            "fill": "#FFFFFF", "y": 0,
            "timelines": [{ "name": "in", "autoplay": true, "loop": true,
              "tracks": [{ "property": "y", "keys": [{"t":0,"v":0},{"t":0.5,"v":30}] }] }] }] }
      ] }"##;
    let script = [
        (0.29, Input::Trigger("two")),
        (0.83, Input::Trigger("one")),
        (1.41, Input::Trigger("two")),
    ];
    same_at_any_rate(
        "scenes",
        s,
        &script,
        &[0.1, 0.29, 0.4, 0.7, 0.83, 1.0, 1.2, 1.41, 1.7, 2.2],
    );
}

/// What this file maps, and what it does not.
///
/// **Mapped.** Timeline keys and every easing; `delay`, `repeat`, `loop`,
/// `hold`; chains by `on_end`, including links that straddle a frame
/// boundary; timelines started part way through a frame. Bindings plain,
/// scaled, offset, thresholded, debounced, and with a transition, in
/// numbers and in colour. Variables moved at instants no frame lands on.
/// Media position, `delay`, `repeat`, `loop`, every `retrigger` with
/// several `voices`, chains by `on_end`, `rest`, and picking from a list.
/// Ducking under a bus, including a ramp turned round halfway. Reel cells
/// with stagger. A counter stepping through whole numbers. Scenes entered
/// part way through a frame.
///
/// **Not mapped, and why.**
///
/// - *Anything a renderer does.* Shapes, paths, transforms, text
///   rasterising, gradients, dots, segments, blends. State is compared
///   with [`Engine::values`], one level below the draw list, because the
///   question here is what the model produced, not how it is drawn.
/// - *Assets.* Images, fonts, vectors and decoded video are host state.
///   The model knows a video only by its length and size.
/// - *A show with more than [`MAX_SUBSTEPS`] endings inside one frame.*
///   The rest of that frame is taken in one piece, so the frame rate
///   shows through. It needs a hundred thousand endings in a frame.
/// - *The host's own clock.* `advance_frame(dt)` adds a delta to where
///   the clock is, so two hosts sampling the same show at different rates
///   land a rounding error apart. `advance_to(instant)` is exact and is
///   what everything here uses.
/// - *Live input.* A show driven by something that has not happened yet
///   is only reproducible once its input is recorded.
#[allow(dead_code)]
const COVERED: () = ();
