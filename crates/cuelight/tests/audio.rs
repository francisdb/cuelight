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

/// One layer, three recordings of the same thing: what a knock, a bumper
/// or a footstep needs so it does not sound like a tape loop.
const KNOCKS: &str = r#"{ "name": "knock", "type": "audio",
    "sound": ["knock1", "knock2", "knock3"], "trigger": "hit" }"#;

fn knocking(pick: &str, seed: u64) -> Engine {
    let mut engine = Engine::new();
    for name in ["knock1", "knock2", "knock3"] {
        engine.set_sound(name, 1.0).unwrap();
    }
    engine.set_seed(seed);
    let layer = KNOCKS.replace("\"trigger\"", &format!("\"pick\": \"{pick}\", \"trigger\""));
    engine.load_show(&show(&layer)).unwrap();
    engine
}

/// The sound of each of `plays` triggers, one play at a time.
fn heard(engine: &mut Engine, plays: usize) -> Vec<String> {
    (0..plays)
        .map(|_| {
            engine.trigger("hit");
            engine.advance_frame(0.0);
            let sound = voices(engine)[0].sound.clone();
            // Let it finish, so each trigger is a play of its own.
            engine.advance_frame(1.5);
            sound
        })
        .collect()
}

#[test]
fn a_layer_of_several_sounds_plays_them_in_turn() {
    let mut engine = knocking("in_order", 0);
    assert_eq!(
        heard(&mut engine, 5),
        ["knock1", "knock2", "knock3", "knock1", "knock2"],
        "a list plays in order and wraps"
    );
}

#[test]
fn picking_is_the_same_every_run_and_differs_by_seed() {
    let first = heard(&mut knocking("random", 7), 12);
    assert_eq!(
        first,
        heard(&mut knocking("random", 7), 12),
        "the same show and seed play the same way twice"
    );
    assert_ne!(
        first,
        heard(&mut knocking("random", 8), 12),
        "a different seed picks differently"
    );
    assert!(
        first.iter().collect::<std::collections::HashSet<_>>().len() > 1,
        "random picked the same sound every time: {first:?}"
    );
}

#[test]
fn shuffle_gives_everything_a_turn_before_repeating() {
    let heard = heard(&mut knocking("shuffle", 3), 9);
    for round in heard.chunks(3) {
        let mut round = round.to_vec();
        round.sort();
        assert_eq!(round, ["knock1", "knock2", "knock3"], "{heard:?}");
    }
}

#[test]
fn two_layers_of_the_same_sounds_do_not_pick_in_step() {
    let mut engine = Engine::new();
    for name in ["knock1", "knock2", "knock3"] {
        engine.set_sound(name, 1.0).unwrap();
    }
    let one = KNOCKS.replace("\"trigger\"", "\"pick\": \"random\", \"trigger\"");
    let two = one.replace("\"knock\"", "\"knock-too\"");
    engine.load_show(&show(&format!("{one}, {two}"))).unwrap();
    let mut apart = 0;
    for _ in 0..12 {
        engine.trigger("hit");
        engine.advance_frame(0.0);
        let heard = voices(&engine);
        assert_eq!(heard.len(), 2);
        if heard[0].sound != heard[1].sound {
            apart += 1;
        }
        engine.advance_frame(1.5);
    }
    assert!(apart > 0, "both layers picked the same sound every time");
}

#[test]
fn rest_drops_a_trigger_that_comes_too_soon() {
    let mut engine = Engine::new();
    engine.set_sound("tick", 0.1).unwrap();
    engine
        .load_show(&show(
            r#"{ "name": "tick", "type": "audio", "sound": "tick",
                 "trigger": "hit", "rest": 1.0 }"#,
        ))
        .unwrap();
    engine.trigger("hit");
    engine.advance_frame(0.2);
    let first = voices(&engine);
    assert!(first.is_empty(), "a 0.1s sound is over by 0.2s");

    // Well inside the rest: dropped, even though nothing is playing.
    engine.trigger("hit");
    engine.advance_frame(0.0);
    assert!(
        voices(&engine).is_empty(),
        "a trigger inside the rest played"
    );

    // Past it: it plays again.
    engine.advance_frame(1.0);
    engine.trigger("hit");
    engine.advance_frame(0.0);
    assert_eq!(voices(&engine).len(), 1);
}

#[test]
fn a_show_says_whether_it_can_make_a_sound() {
    let mut engine = Engine::new();
    engine.set_sound("thunder", 2.0).unwrap();

    engine
        .load_show(
            r##"{ "name": "quiet", "size": [64, 32],
                 "layers": [ { "name": "box", "type": "shape", "fill": "#FFFFFF",
                               "shape": { "rect": [0, 0, 8, 8] } } ] }"##,
        )
        .unwrap();
    assert!(
        !engine.show().unwrap().has_sound(),
        "nothing here can be heard, so a host need not open a device"
    );

    // Buried in a group, and in a scene rather than the show's layers.
    engine
        .load_show(
            r#"{ "name": "loud", "size": [64, 32],
                 "scenes": [ { "name": "game", "trigger": "start", "layers": [
                   { "name": "group", "type": "group", "children": [
                     { "name": "thunder", "type": "audio", "sound": "thunder",
                       "trigger": "strike" } ] } ] } ] }"#,
        )
        .unwrap();
    assert!(engine.show().unwrap().has_sound());
}

#[test]
fn a_video_is_a_sound_only_once_a_host_hands_one_over() {
    // A clip is heard when the host registers a sound under the video's
    // name, which is how it says the clip has a soundtrack and hands over
    // its samples. Until then a video layer makes no voices, so a show
    // that is only video needs no sound device.
    let mut engine = Engine::new();
    engine.set_video("intro", 2.0, [16.0, 8.0]).unwrap();
    let show = r#"{ "name": "screen", "size": [64, 32],
                    "layers": [ { "name": "intro", "type": "video", "video": "intro",
                                  "autoplay": true } ] }"#;
    engine.load_show(show).unwrap();
    engine.advance_frame(0.5);
    assert!(engine.voices().unwrap().is_empty());
    assert!(
        !engine.show().unwrap().has_sound(),
        "a show's own document cannot promise a soundtrack a host may never register"
    );

    // Hand one over and the same show is heard.
    engine.set_sound("intro", 2.0).unwrap();
    engine.load_show(show).unwrap();
    engine.advance_frame(0.5);
    assert_eq!(engine.voices().unwrap().len(), 1);
}

#[test]
fn a_bound_sound_plays_what_it_is_pointed_at() {
    let show = r#"{
      "name": "bed", "size": [8, 8],
      "variables": { "mode": "" },
      "layers": [
        { "name": "bed", "type": "audio", "sound": "idle", "loop": true,
          "bindings": [{ "property": "sound", "variable": "mode",
                         "map": { "play": "theme", "over": "quiet" } }] }
      ]
    }"#;
    let mut engine = Engine::new();
    for name in ["idle", "theme", "quiet"] {
        engine.set_sound(name, 4.0).unwrap();
    }
    engine.load_show(show).unwrap();

    // Not told yet: a pointed layer that has heard nothing does not play,
    // the same rule a video layer follows.
    engine.advance_frame(0.0);
    assert!(engine.voices().unwrap().is_empty());

    engine.set_variable("mode", "play");
    engine.advance_frame(0.016);
    let voices = engine.voices().unwrap();
    assert_eq!(voices.len(), 1);
    assert_eq!(voices[0].sound, "theme");
    assert!(voices[0].looping);

    // Pointed somewhere else mid-play: the new one from the top, keeping
    // the loop. That is what a play is, so `retrigger` governs it.
    engine.advance_frame(1.0);
    engine.set_variable("mode", "over");
    engine.advance_frame(0.016);
    let voices = engine.voices().unwrap();
    assert_eq!(voices.len(), 1);
    assert_eq!(voices[0].sound, "quiet");
    assert!(voices[0].position < 0.1, "{:?}", voices[0].position);

    // A value the map does not list says nothing, so the bed holds.
    engine.set_variable("mode", "elsewhere");
    engine.advance_frame(0.016);
    assert_eq!(engine.voices().unwrap()[0].sound, "quiet");
}

#[test]
fn sound_is_not_a_property_of_other_layers() {
    let show = r#"{
      "name": "no", "size": [8, 8],
      "layers": [
        { "name": "box", "type": "shape", "shape": { "kind": "rect", "width": 2, "height": 2 },
          "bindings": [{ "property": "sound", "variable": "x" }] }
      ]
    }"#;
    assert!(Engine::new().load_show(show).is_err());
}
