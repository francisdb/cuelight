//! Video layers: a playhead the host decodes for, and the frame it draws.

use cuelight::{Engine, Event, ResolvedShape};

fn show(layers: &str) -> String {
    format!(r#"{{ "name": "video", "size": [64, 32], "layers": [{layers}] }}"#)
}

/// An engine with a 2 second, 16x8 video registered.
fn engine(layers: &str) -> Engine {
    let mut engine = Engine::new();
    engine.set_video("intro", 2.0, [16.0, 8.0]).unwrap();
    engine.load_show(&show(layers)).unwrap();
    engine
}

/// A frame of flat colour, as a host would hand over after decoding.
fn frame(engine: &mut Engine, name: &str, [width, height]: [u32; 2]) {
    let pixels = vec![255; (width * height * 4) as usize];
    engine.set_image(name, width, height, pixels).unwrap();
}

const INTRO: &str = r#"{ "name": "intro", "type": "video", "video": "intro",
    "trigger": "play", "stop": "cut", "on_end": "intro_done" }"#;

#[test]
fn a_video_plays_on_its_trigger_and_reports_its_position() {
    let mut engine = engine(INTRO);
    assert!(engine.videos().unwrap().is_empty());
    assert!(engine.show().unwrap().triggers().contains("play"));

    engine.trigger("play");
    let playing = engine.videos().unwrap();
    assert_eq!(playing.len(), 1);
    assert_eq!(playing[0].layer, "intro");
    assert_eq!(playing[0].video, "intro");
    assert_eq!(playing[0].position, 0.0);
    assert!(!playing[0].looping);

    engine.advance_frame(0.5);
    assert!((engine.videos().unwrap()[0].position - 0.5).abs() < 1e-9);
    assert!(engine.drain_events().is_empty());

    engine.advance_frame(1.5);
    assert!(engine.videos().unwrap().is_empty());
    assert_eq!(
        engine.drain_events(),
        vec![Event::Trigger("intro_done".into())]
    );
}

#[test]
fn the_engine_decodes_nothing_and_draws_what_the_host_hands_over() {
    let mut engine = engine(INTRO);
    engine.trigger("play");
    // No frame yet: the layer draws nothing, but it is still playing.
    assert!(engine.resolved_layers().unwrap().is_empty());
    assert_eq!(engine.videos().unwrap().len(), 1);

    frame(&mut engine, "intro", [16, 8]);
    let layers = engine.resolved_layers().unwrap();
    assert_eq!(layers.len(), 1);
    match &layers[0].shape {
        ResolvedShape::Image {
            image,
            width,
            height,
            ..
        } => {
            assert_eq!(image, "intro");
            // The registered size, not the frame's, so a host may hand
            // over a frame of any size.
            assert_eq!((*width, *height), (16.0, 8.0));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn size_scales_the_picture_like_an_image() {
    let mut engine = engine(
        r#"{ "name": "intro", "type": "video", "video": "intro", "autoplay": true,
             "size": [32, 16], "x": 4, "y": 2 }"#,
    );
    frame(&mut engine, "intro", [16, 8]);
    let ResolvedShape::Image {
        x,
        y,
        width,
        height,
        ..
    } = engine.resolved_layers().unwrap()[0].shape
    else {
        panic!("a video draws a picture")
    };
    assert_eq!((x, y, width, height), (4.0, 2.0, 32.0, 16.0));
}

#[test]
fn a_loop_wraps_and_never_ends() {
    let mut engine = engine(
        r#"{ "name": "bed", "type": "video", "video": "intro", "autoplay": true,
             "loop": true, "on_end": "never" }"#,
    );
    engine.advance_frame(2.5);
    let playing = engine.videos().unwrap();
    assert!(playing[0].looping);
    assert!((playing[0].position - 0.5).abs() < 1e-9);
    assert!(engine.drain_events().is_empty());
}

#[test]
fn a_trigger_restarts_a_video_rather_than_overlapping_it() {
    let mut engine = engine(INTRO);
    engine.trigger("play");
    engine.advance_frame(1.0);
    let first = engine.videos().unwrap()[0].id;
    engine.trigger("play");
    let playing = engine.videos().unwrap();
    // One picture at a time, from the top.
    assert_eq!(playing.len(), 1);
    assert_ne!(playing[0].id, first);
    assert_eq!(playing[0].position, 0.0);
}

#[test]
fn stop_and_scene_changes_end_a_play() {
    let mut engine = engine(INTRO);
    engine.trigger("play");
    engine.advance_frame(0.2);
    engine.trigger("cut");
    assert!(engine.videos().unwrap().is_empty());
    engine.advance_frame(5.0);
    assert!(engine.drain_events().is_empty(), "a cut fires no on_end");
}

#[test]
fn a_video_registered_late_is_shown_from_where_it_would_be() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r#"{ "name": "late", "type": "video", "video": "late", "autoplay": true,
                 "on_end": "done" }"#,
        ))
        .unwrap();
    engine.advance_frame(1.0);
    // Unknown length: it plays on rather than ending at a guess.
    assert!(engine.videos().unwrap().is_empty());
    assert!(engine.drain_events().is_empty());
    engine.set_video("late", 3.0, [8.0, 8.0]).unwrap();
    assert!((engine.videos().unwrap()[0].position - 1.0).abs() < 1e-9);
    engine.advance_frame(2.0);
    assert!(engine.videos().unwrap().is_empty());
    assert_eq!(engine.drain_events(), vec![Event::Trigger("done".into())]);
}

#[test]
fn an_invisible_video_is_not_shown() {
    let mut engine = engine(
        r#"{ "name": "intro", "type": "video", "video": "intro", "autoplay": true,
             "visible": false }"#,
    );
    frame(&mut engine, "intro", [16, 8]);
    assert!(engine.videos().unwrap().is_empty());
    assert!(engine.resolved_layers().unwrap().is_empty());
}

#[test]
fn rejects_bad_videos() {
    let bad = |layer: &str, expect: &str| {
        let err = Engine::new()
            .load_show(&show(layer))
            .unwrap_err()
            .to_string();
        assert!(err.contains(expect), "{err}");
    };
    bad(
        r#"{ "name": "v", "type": "video", "video": "x", "loop": true, "repeat": 2 }"#,
        "both loop and repeat",
    );
    bad(
        r#"{ "name": "v", "type": "video", "video": "x", "delay": -1 }"#,
        "delay",
    );
    assert!(Engine::new().set_video("x", 0.0, [4.0, 4.0]).is_err());
    assert!(Engine::new().set_video("x", 1.0, [0.0, 4.0]).is_err());
}

#[test]
fn a_bound_name_lets_one_layer_show_anything() {
    let mut engine = Engine::new();
    engine.set_video("intro", 2.0, [16.0, 8.0]).unwrap();
    engine.set_video("bonus", 4.0, [32.0, 16.0]).unwrap();
    engine
        .load_show(
            r#"{ "name": "playing", "size": [64, 32], "variables": { "clip": "intro" },
                 "layers": [ { "name": "picture", "type": "video", "video": "intro",
                               "autoplay": true,
                               "bindings": [ { "property": "video", "variable": "clip" } ] } ] }"#,
        )
        .unwrap();
    engine.advance_frame(0.5);
    let playing = engine.videos().unwrap();
    assert_eq!(playing[0].video, "intro");
    assert!((playing[0].position - 0.5).abs() < 1e-9);
    let first = playing[0].id;

    // Point it at another clip: that one plays, from the top.
    engine.set_variable("clip", "bonus");
    engine.advance_frame(0.25);
    let playing = engine.videos().unwrap();
    assert_eq!(playing.len(), 1, "one layer, one picture");
    assert_eq!(playing[0].video, "bonus");
    assert!((playing[0].position - 0.25).abs() < 1e-9, "{playing:?}");
    assert_ne!(playing[0].id, first, "a new play, so a host starts over");
}

#[test]
fn a_layer_draws_and_measures_the_clip_it_is_pointed_at() {
    let mut engine = Engine::new();
    engine.set_video("intro", 2.0, [16.0, 8.0]).unwrap();
    engine.set_video("bonus", 4.0, [32.0, 16.0]).unwrap();
    engine
        .load_show(
            r#"{ "name": "playing", "size": [64, 32], "variables": { "clip": "intro" },
                 "layers": [ { "name": "picture", "type": "video", "video": "intro",
                               "autoplay": true,
                               "bindings": [ { "property": "video", "variable": "clip" } ] } ] }"#,
        )
        .unwrap();
    frame(&mut engine, "intro", [16, 8]);
    frame(&mut engine, "bonus", [32, 16]);
    let drawn = |e: &Engine| match &e.resolved_layers().unwrap()[0].shape {
        ResolvedShape::Image {
            image,
            width,
            height,
            ..
        } => (image.clone(), *width, *height),
        other => panic!("{other:?}"),
    };
    assert_eq!(drawn(&engine), ("intro".to_owned(), 16.0, 8.0));
    engine.set_variable("clip", "bonus");
    engine.advance_frame(0.0);
    // The other clip, at its own size.
    assert_eq!(drawn(&engine), ("bonus".to_owned(), 32.0, 16.0));
}

#[test]
fn a_layer_ends_on_the_clip_it_is_playing() {
    let mut engine = Engine::new();
    engine.set_video("short", 1.0, [8.0, 8.0]).unwrap();
    engine.set_video("long", 10.0, [8.0, 8.0]).unwrap();
    engine
        .load_show(
            r#"{ "name": "playing", "size": [64, 32], "variables": { "clip": "long" },
                 "layers": [ { "name": "picture", "type": "video", "video": "long",
                               "autoplay": true, "on_end": "done",
                               "bindings": [ { "property": "video", "variable": "clip" } ] } ] }"#,
        )
        .unwrap();
    engine.advance_frame(2.0);
    assert!(
        !engine.videos().unwrap().is_empty(),
        "the long one plays on"
    );
    // Switched to a short clip: it ends after its own length, not the
    // length of the one it replaced.
    engine.set_variable("clip", "short");
    engine.advance_frame(0.5);
    assert!(!engine.videos().unwrap().is_empty());
    engine.advance_frame(0.6);
    assert!(engine.videos().unwrap().is_empty());
    assert_eq!(engine.drain_events(), vec![Event::Trigger("done".into())]);
}

#[test]
fn a_video_that_has_ended_shows_nothing() {
    let mut engine = engine(INTRO);
    engine.trigger("play");
    frame(&mut engine, "intro", [16, 8]);
    assert_eq!(engine.resolved_layers().unwrap().len(), 1);
    // Past its end: the layer is done, so a background behind it shows
    // through rather than its last frame staying visible.
    engine.advance_frame(3.0);
    assert!(engine.videos().unwrap().is_empty());
    assert!(
        engine.resolved_layers().unwrap().is_empty(),
        "a finished video kept drawing its last frame"
    );
}

#[test]
fn an_idle_layer_pointed_at_a_clip_plays_it() {
    let mut engine = Engine::new();
    engine.set_video("short", 1.0, [8.0, 8.0]).unwrap();
    engine.set_video("other", 5.0, [8.0, 8.0]).unwrap();
    engine
        .load_show(
            r#"{ "name": "playing", "size": [64, 32], "variables": { "clip": "short" },
                 "layers": [ { "name": "picture", "type": "video", "video": "short",
                               "autoplay": true,
                               "bindings": [ { "property": "video", "variable": "clip" } ] } ] }"#,
        )
        .unwrap();
    engine.advance_frame(1.5);
    assert!(engine.videos().unwrap().is_empty(), "the short clip ended");

    // The layer sat idle; naming another clip starts it, as asking for a
    // clip is the whole of what a host says.
    engine.set_variable("clip", "other");
    engine.advance_frame(0.25);
    let playing = engine.videos().unwrap();
    assert_eq!(playing.len(), 1);
    assert_eq!(playing[0].video, "other");
    assert!((playing[0].position - 0.25).abs() < 1e-9, "{playing:?}");
}

#[test]
fn a_layer_of_several_clips_shows_the_next_one_each_play() {
    let mut engine = Engine::new();
    for name in ["one", "two", "three"] {
        engine.set_video(name, 1.0, [8.0, 8.0]).unwrap();
    }
    engine
        .load_show(
            r#"{ "name": "between", "size": [64, 32],
                 "layers": [ { "name": "filler", "type": "video",
                               "video": ["one", "two", "three"],
                               "trigger": "next" } ] }"#,
        )
        .unwrap();
    let mut shown = Vec::new();
    for _ in 0..4 {
        engine.trigger("next");
        engine.advance_frame(0.0);
        shown.push(engine.videos().unwrap()[0].video.clone());
        engine.advance_frame(1.5);
    }
    assert_eq!(shown, ["one", "two", "three", "one"]);
    // It waits to be played: a list is not a playlist that runs on by
    // itself once the first clip ends.
    engine.advance_frame(5.0);
    assert!(engine.videos().unwrap().is_empty());
}

#[test]
fn a_layer_that_is_pointed_somewhere_ignores_its_own_list() {
    let mut engine = Engine::new();
    for name in ["one", "two", "asked"] {
        engine.set_video(name, 1.0, [8.0, 8.0]).unwrap();
    }
    engine
        .load_show(
            r#"{ "name": "playing", "size": [64, 32], "variables": { "clip": "asked" },
                 "layers": [ { "name": "picture", "type": "video", "video": ["one", "two"],
                               "trigger": "go",
                               "bindings": [ { "property": "video", "variable": "clip" } ] } ] }"#,
        )
        .unwrap();
    engine.trigger("go");
    engine.advance_frame(0.0);
    assert_eq!(engine.videos().unwrap()[0].video, "asked");
}

#[test]
fn queue_lets_a_clip_finish_then_plays_the_next() {
    let mut engine = Engine::new();
    engine.set_video("one", 1.0, [8.0, 8.0]).unwrap();
    engine.set_video("two", 1.0, [8.0, 8.0]).unwrap();
    engine
        .load_show(
            r#"{ "name": "playing", "size": [64, 32], "variables": { "clip": "one" },
                 "layers": [ { "name": "picture", "type": "video", "video": "one",
                               "trigger": "go", "retrigger": "queue",
                               "bindings": [ { "property": "video", "variable": "clip" } ] } ] }"#,
        )
        .unwrap();
    engine.trigger("go");
    engine.advance_frame(0.4);
    // A second call while the first plays waits rather than cutting in.
    engine.set_variable("clip", "two");
    engine.trigger("go");
    engine.advance_frame(0.0);
    let playing = engine.videos().unwrap();
    assert_eq!(playing.len(), 1, "one layer shows one picture");
    assert_eq!(playing[0].video, "one", "the first clip was cut off");

    // Once the first is done, the one that waited plays, from the top.
    engine.advance_frame(0.7);
    let playing = engine.videos().unwrap();
    assert_eq!(playing[0].video, "two");
    assert!(playing[0].position < 0.2, "{playing:?}");
}

#[test]
fn ignore_drops_a_trigger_that_arrives_mid_clip() {
    let mut engine = Engine::new();
    engine.set_video("one", 1.0, [8.0, 8.0]).unwrap();
    engine.set_video("two", 1.0, [8.0, 8.0]).unwrap();
    engine
        .load_show(
            r#"{ "name": "playing", "size": [64, 32], "variables": { "clip": "one" },
                 "layers": [ { "name": "picture", "type": "video", "video": "one",
                               "trigger": "go", "retrigger": "ignore",
                               "bindings": [ { "property": "video", "variable": "clip" } ] } ] }"#,
        )
        .unwrap();
    engine.trigger("go");
    engine.advance_frame(0.4);
    engine.set_variable("clip", "two");
    engine.trigger("go");
    engine.advance_frame(0.0);
    assert_eq!(engine.videos().unwrap()[0].video, "one");
    // Dropped, not queued: nothing follows.
    engine.advance_frame(0.7);
    assert!(engine.videos().unwrap().is_empty());
}

#[test]
fn a_video_layer_cannot_overlap_itself() {
    let mut engine = Engine::new();
    engine.set_video("one", 1.0, [8.0, 8.0]).unwrap();
    let e = engine
        .load_show(
            r#"{ "name": "playing", "size": [64, 32],
                 "layers": [ { "name": "picture", "type": "video", "video": "one",
                               "trigger": "go", "retrigger": "overlap" } ] }"#,
        )
        .unwrap_err();
    assert!(format!("{e}").contains("one picture at a time"), "{e}");
}

#[test]
fn a_default_pick_is_not_reported_as_an_unknown_field() {
    let mut engine = Engine::new();
    engine.set_video("one", 1.0, [8.0, 8.0]).unwrap();
    engine
        .load_show(
            r#"{ "name": "playing", "size": [64, 32],
                 "layers": [ { "name": "picture", "type": "video", "video": "one",
                               "pick": "in_order", "retrigger": "restart",
                               "rest": 0, "trigger": "go" } ] }"#,
        )
        .unwrap();
    assert!(
        engine.load_warnings().is_empty(),
        "{:?}",
        engine.load_warnings()
    );
}

/// An engine with a 2 second clip whose soundtrack the host has also
/// registered, which is how a host says a clip has one.
fn with_sound(layers: &str) -> Engine {
    let mut engine = Engine::new();
    engine.set_video("intro", 2.0, [16.0, 8.0]).unwrap();
    engine.set_sound("intro", 2.0).unwrap();
    engine.load_show(&show(layers)).unwrap();
    engine
}

#[test]
fn a_clip_with_a_soundtrack_is_heard_while_it_plays() {
    let mut engine = with_sound(INTRO);
    assert!(engine.voices().unwrap().is_empty(), "nothing plays yet");

    engine.trigger("play");
    engine.advance_frame(0.5);
    let heard = engine.voices().unwrap();
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].sound, "intro");
    assert_eq!(heard[0].layer, "intro");
    assert!((heard[0].position - 0.5).abs() < 1e-9);
    assert!((heard[0].gain - 1.0).abs() < 1e-9);

    // One play, one id: the picture and the sound are the same play, so a
    // host can see they belong together.
    assert_eq!(heard[0].id, engine.videos().unwrap()[0].id);
    assert!((heard[0].position - engine.videos().unwrap()[0].position).abs() < 1e-9);

    // It ends with the picture.
    engine.advance_frame(2.0);
    assert!(engine.voices().unwrap().is_empty());
}

#[test]
fn a_clip_the_host_registered_no_sound_for_stays_silent() {
    // The tripwire the other way round: a clip is heard only when the host
    // has handed over a soundtrack for it.
    let mut engine = engine(INTRO);
    engine.trigger("play");
    engine.advance_frame(0.5);
    assert_eq!(engine.videos().unwrap().len(), 1, "the picture plays");
    assert!(engine.voices().unwrap().is_empty(), "and makes no sound");
}

#[test]
fn a_clips_sound_follows_its_picture_round_a_loop() {
    let mut engine = with_sound(
        r#"{ "name": "intro", "type": "video", "video": "intro",
             "trigger": "play", "loop": true }"#,
    );
    engine.trigger("play");
    engine.advance_frame(2.5);
    let heard = engine.voices().unwrap();
    assert_eq!(heard.len(), 1);
    assert!(heard[0].looping);
    // Wrapped on the picture's length, not the soundtrack's, so the two
    // cannot drift apart over a long loop.
    assert!((heard[0].position - 0.5).abs() < 1e-9, "{heard:?}");
    assert!((heard[0].position - engine.videos().unwrap()[0].position).abs() < 1e-9);
}

#[test]
fn a_clip_can_be_played_silently() {
    let mut engine = with_sound(
        r#"{ "name": "intro", "type": "video", "video": "intro",
             "trigger": "play", "gain": 0 }"#,
    );
    engine.trigger("play");
    engine.advance_frame(0.5);
    assert_eq!(
        engine.voices().unwrap()[0].gain,
        0.0,
        "a backdrop should be able to keep its pictures and lose its sound"
    );
    assert_eq!(engine.videos().unwrap().len(), 1, "the picture plays on");
}

#[test]
fn a_group_turns_down_the_clips_below_it() {
    let mut engine = with_sound(
        r#"{ "name": "room", "type": "group", "gain": 0.5, "children": [
             { "name": "intro", "type": "video", "video": "intro",
               "trigger": "play", "gain": 0.8, "bus": "music" } ] }"#,
    );
    engine.trigger("play");
    engine.advance_frame(0.25);
    let heard = engine.voices().unwrap();
    assert!((heard[0].gain - 0.4).abs() < 1e-9, "{heard:?}");
    assert_eq!(heard[0].bus.as_deref(), Some("music"));
}
