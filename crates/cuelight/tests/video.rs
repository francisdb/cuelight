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
