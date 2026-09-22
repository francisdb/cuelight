//! Audio layers as pure data: plays, positions, gains and ends, no device.

use cuelight::{Engine, Event, Voice};

fn show(layers: &str) -> String {
    format!(
        r#"{{ "name": "sound", "size": [64, 32], "variables": {{ "level": 1 }}, "layers": [{layers}] }}"#
    )
}

fn engine(layers: &str) -> Engine {
    let mut engine = Engine::new();
    engine.set_sound("thunder", 2.0).unwrap();
    engine.set_sound("loop", 0.5).unwrap();
    engine.load_show(&show(layers)).unwrap();
    engine
}

fn voices(engine: &Engine) -> Vec<Voice> {
    engine.voices().unwrap()
}

const THUNDER: &str = r#"{ "name": "thunder", "type": "audio", "sound": "thunder",
    "trigger": "strike", "stop": "hush", "gain": 0.8, "bus": "sfx", "on_end": "thunder_done" }"#;

#[test]
fn trigger_plays_a_sound_until_it_ends() {
    let mut engine = engine(THUNDER);
    assert!(voices(&engine).is_empty());
    assert!(engine.show().unwrap().triggers().contains("strike"));
    assert!(engine.show().unwrap().triggers().contains("hush"));

    engine.trigger("strike");
    let v = voices(&engine);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].layer, "thunder");
    assert_eq!(v[0].sound, "thunder");
    assert_eq!(v[0].position, 0.0);
    assert!((v[0].gain - 0.8).abs() < 1e-9);
    assert_eq!(v[0].bus.as_deref(), Some("sfx"));
    assert!(!v[0].looping);

    engine.advance_frame(0.5);
    assert!((voices(&engine)[0].position - 0.5).abs() < 1e-9);
    assert!(engine.drain_events().is_empty());

    engine.advance_frame(1.5);
    assert!(voices(&engine).is_empty());
    assert_eq!(
        engine.drain_events(),
        vec![Event::Trigger("thunder_done".into())]
    );
}

#[test]
fn stop_trigger_ends_the_play_without_on_end() {
    let mut engine = engine(THUNDER);
    engine.trigger("strike");
    engine.advance_frame(0.1);
    engine.trigger("hush");
    assert!(voices(&engine).is_empty());
    engine.advance_frame(5.0);
    assert!(engine.drain_events().is_empty());
}

#[test]
fn audio_draws_nothing() {
    let engine = engine(THUNDER);
    assert!(engine.resolved_layers().unwrap().is_empty());
}

#[test]
fn restart_is_the_default_retrigger() {
    let mut engine = engine(THUNDER);
    engine.trigger("strike");
    engine.advance_frame(1.0);
    let first = voices(&engine)[0].id;
    engine.trigger("strike");
    let v = voices(&engine);
    assert_eq!(v.len(), 1);
    assert_ne!(v[0].id, first);
    assert_eq!(v[0].position, 0.0);
}

#[test]
fn overlap_keeps_the_newest_voices() {
    let mut engine = engine(
        r#"{ "name": "flap", "type": "audio", "sound": "thunder", "trigger": "flap",
             "retrigger": "overlap", "voices": 2 }"#,
    );
    engine.trigger("flap");
    engine.advance_frame(0.1);
    engine.trigger("flap");
    engine.advance_frame(0.1);
    let two: Vec<u64> = voices(&engine).iter().map(|v| v.id).collect();
    assert_eq!(two.len(), 2);
    engine.trigger("flap");
    let v = voices(&engine);
    assert_eq!(v.len(), 2);
    // The oldest went; the ids stay in start order.
    assert_eq!(v[0].id, two[1]);
    assert!((v[0].position - 0.1).abs() < 1e-9);
    assert_eq!(v[1].position, 0.0);
}

#[test]
fn ignore_lets_the_play_finish() {
    let mut engine = engine(
        r#"{ "name": "once", "type": "audio", "sound": "thunder", "trigger": "go",
             "retrigger": "ignore" }"#,
    );
    engine.trigger("go");
    engine.advance_frame(1.0);
    engine.trigger("go");
    let v = voices(&engine);
    assert_eq!(v.len(), 1);
    assert!((v[0].position - 1.0).abs() < 1e-9);
    engine.advance_frame(1.0);
    assert!(voices(&engine).is_empty());
    engine.trigger("go");
    assert_eq!(voices(&engine).len(), 1);
}

#[test]
fn autoplay_loops_wrap_and_never_end() {
    let mut engine = engine(
        r#"{ "name": "hum", "type": "audio", "sound": "loop", "autoplay": true, "loop": true,
             "on_end": "never" }"#,
    );
    let v = voices(&engine);
    assert_eq!(v.len(), 1);
    assert!(v[0].looping);
    engine.advance_frame(1.2);
    assert!((voices(&engine)[0].position - 0.2).abs() < 1e-9);
    assert!(engine.drain_events().is_empty());
}

#[test]
fn delay_and_repeat_as_on_timelines() {
    let mut engine = engine(
        r#"{ "name": "two", "type": "audio", "sound": "loop", "trigger": "go",
             "delay": 0.25, "repeat": 2, "on_end": "done" }"#,
    );
    engine.trigger("go");
    // Delayed: nothing to hear yet.
    assert!(voices(&engine).is_empty());
    engine.advance_frame(0.25);
    assert_eq!(voices(&engine)[0].position, 0.0);
    engine.advance_frame(0.6);
    // Second play, 0.1 in.
    assert!((voices(&engine)[0].position - 0.1).abs() < 1e-9);
    engine.advance_frame(0.4);
    assert!(voices(&engine).is_empty());
    assert_eq!(engine.drain_events(), vec![Event::Trigger("done".into())]);
}

#[test]
fn gain_multiplies_down_the_tree_and_can_be_bound() {
    let mut engine = engine(
        r#"{ "name": "mix", "type": "group", "gain": 0.5, "children": [
               { "name": "hum", "type": "audio", "sound": "loop", "autoplay": true, "loop": true,
                 "bindings": [ { "property": "gain", "variable": "level", "scale": 0.5 } ] } ] }"#,
    );
    assert!((voices(&engine)[0].gain - 0.25).abs() < 1e-9);
    engine.set_variable("level", 2.0);
    assert!((voices(&engine)[0].gain - 0.5).abs() < 1e-9);
    engine.set_variable("level", -3.0);
    assert_eq!(voices(&engine)[0].gain, 0.0);
}

#[test]
fn gain_can_be_animated() {
    let mut engine = engine(
        r#"{ "name": "hum", "type": "audio", "sound": "loop", "autoplay": true, "loop": true,
             "timelines": [ { "name": "fade", "trigger": "fade",
               "tracks": [ { "property": "gain", "keys": [ { "t": 0, "v": 1 }, { "t": 1, "v": 0 } ] } ] } ] }"#,
    );
    engine.trigger("fade");
    engine.advance_frame(0.5);
    assert!((voices(&engine)[0].gain - 0.5).abs() < 1e-9);
    engine.advance_frame(1.0);
    assert_eq!(voices(&engine)[0].gain, 1.0);
}

#[test]
fn gain_is_only_for_groups_and_audio() {
    let mut engine = Engine::new();
    let err = engine
        .load_show(&show(
            r##"{ "name": "box", "type": "shape", "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF",
                 "bindings": [ { "property": "gain", "variable": "level" } ] }"##,
        ))
        .unwrap_err();
    assert!(err.to_string().contains("no Gain property"), "{err}");
}

#[test]
fn invisible_layers_are_not_heard() {
    let engine = engine(
        r#"{ "name": "hum", "type": "audio", "sound": "loop", "autoplay": true, "loop": true,
             "visible": false }"#,
    );
    assert!(voices(&engine).is_empty());
}

#[test]
fn scenes_own_their_sounds() {
    let mut engine = Engine::new();
    engine.set_sound("loop", 0.5).unwrap();
    engine
        .load_show(
            r#"{ "name": "scenes", "size": [64, 32],
                 "layers": [ { "name": "bed", "type": "audio", "sound": "loop", "autoplay": true, "loop": true } ],
                 "scenes": [
                   { "name": "a", "trigger": "a", "layers": [
                       { "name": "a_hum", "type": "audio", "sound": "loop", "autoplay": true, "loop": true } ] },
                   { "name": "b", "trigger": "b", "layers": [
                       { "name": "b_hum", "type": "audio", "sound": "loop", "trigger": "b", "loop": true } ] } ] }"#,
        )
        .unwrap();
    let names = |e: &Engine| -> Vec<String> { voices(e).into_iter().map(|v| v.layer).collect() };
    assert_eq!(names(&engine), ["bed", "a_hum"]);
    engine.advance_frame(0.2);
    // The scene's trigger both enters it and plays the layer listening.
    engine.trigger("b");
    assert_eq!(names(&engine), ["bed", "b_hum"]);
    assert!((voices(&engine)[0].position - 0.2).abs() < 1e-9);
    engine.trigger("a");
    assert_eq!(names(&engine), ["bed", "a_hum"]);
}

#[test]
fn a_sound_registered_late_is_heard_from_where_it_would_be() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r#"{ "name": "hum", "type": "audio", "sound": "late", "autoplay": true, "on_end": "done" }"#,
        ))
        .unwrap();
    engine.advance_frame(1.0);
    assert!(voices(&engine).is_empty());
    assert!(engine.drain_events().is_empty());
    engine.set_sound("late", 3.0).unwrap();
    assert!((voices(&engine)[0].position - 1.0).abs() < 1e-9);
    engine.advance_frame(2.0);
    assert!(voices(&engine).is_empty());
    assert_eq!(engine.drain_events(), vec![Event::Trigger("done".into())]);
}

#[test]
fn rejects_bad_audio_layers() {
    let bad = |layer: &str, expect: &str| {
        let err = Engine::new()
            .load_show(&show(layer))
            .unwrap_err()
            .to_string();
        assert!(err.contains(expect), "{err}");
    };
    bad(
        r#"{ "name": "x", "type": "audio", "sound": "s", "loop": true, "repeat": 2 }"#,
        "both loop and repeat",
    );
    bad(
        r#"{ "name": "x", "type": "audio", "sound": "s", "delay": -1 }"#,
        "delay",
    );
    bad(
        r#"{ "name": "x", "type": "audio", "sound": "s", "retrigger": "overlap", "voices": 0 }"#,
        "voice",
    );
    bad(
        r#"{ "name": "x", "type": "audio", "sound": "s", "anchor": "center" }"#,
        "anchor",
    );
    assert!(Engine::new().set_sound("s", 0.0).is_err());
}
