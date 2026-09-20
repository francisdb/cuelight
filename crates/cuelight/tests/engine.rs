//! Structural tests: the whole trigger/variable/timeline model with no GPU.

use cuelight::{Engine, ResolvedShape};

const MINIGOLF: &str = include_str!("../examples/shows/minigolf.json");

fn ball_x(engine: &Engine) -> f64 {
    let layers = engine.resolved_layers().unwrap();
    let ball = layers.iter().find(|l| l.name == "ball").unwrap();
    match ball.shape {
        ResolvedShape::Circle { cx, .. } => cx,
        _ => panic!("ball should be a circle"),
    }
}

#[test]
fn loads_show_and_resolves_layers() {
    let mut engine = Engine::new();
    engine.load_show(MINIGOLF).unwrap();
    let layers = engine.resolved_layers().unwrap();
    // court, court_right, hole, score_bar, ball, ball_shadow (the golf_ball
    // group flattens into its children)
    assert_eq!(layers.len(), 6);
    assert_eq!(layers[0].name, "court");
    assert_eq!(ball_x(&engine), 64.0);
}

#[test]
fn rejects_invalid_show() {
    let mut engine = Engine::new();
    assert!(engine.load_show("{ not json").is_err());
    assert!(engine.resolved_layers().is_err());
}

#[test]
fn binding_follows_variable() {
    let mut engine = Engine::new();
    engine.load_show(MINIGOLF).unwrap();
    let opacity = |e: &Engine| {
        e.resolved_layers()
            .unwrap()
            .iter()
            .find(|l| l.name == "score_bar")
            .unwrap()
            .opacity
    };
    // score starts at 0: opacity = 0 * 0.001 + 0.2
    assert!((opacity(&engine) - 0.2).abs() < 1e-9);
    engine.set_variable("score", 500.0);
    assert!((opacity(&engine) - 0.7).abs() < 1e-9);
}

#[test]
fn trigger_starts_timeline_and_advance_frame_animates() {
    let mut engine = Engine::new();
    engine.load_show(MINIGOLF).unwrap();

    // Nothing moves before the trigger.
    engine.advance_frame(0.25);
    assert_eq!(ball_x(&engine), 64.0);

    engine.trigger("go");
    assert_eq!(ball_x(&engine), 64.0);

    // Midway: cubic-out at t=0.5 has covered 7/8 of the distance (the
    // ball decelerates into the hole).
    engine.advance_frame(0.5);
    assert!((ball_x(&engine) - (64.0 + (440.0 - 64.0) * 0.875)).abs() < 1e-9);

    // Past the end: the timeline finished, property falls back to base.
    engine.advance_frame(1.0);
    assert_eq!(ball_x(&engine), 64.0);
}

#[test]
fn timeline_end_value_holds_at_duration() {
    let mut engine = Engine::new();
    engine.load_show(MINIGOLF).unwrap();
    engine.trigger("go");
    // Land close to, but not past, the end.
    engine.advance_frame(0.999);
    let x = ball_x(&engine);
    assert!(x > 430.0 && x <= 440.0, "x was {x}");
}

#[test]
fn image_layers_resolve_with_host_pixels() {
    const SCENE: &str = r#"{
        "name": "img",
        "size": [64, 64],
        "layers": [
            { "name": "natural", "type": "image", "image": "tex", "x": 4, "y": 6 },
            { "name": "scaled", "type": "image", "image": "tex", "size": [10, 20] },
            { "name": "missing", "type": "image", "image": "nope" }
        ]
    }"#;
    let mut engine = Engine::new();
    engine.load_show(SCENE).unwrap();

    // No pixels registered yet: image layers are skipped, not an error.
    assert!(engine.resolved_layers().unwrap().is_empty());

    // Wrong buffer size is rejected.
    assert!(engine.set_image("tex", 2, 2, vec![0u8; 3]).is_err());

    engine.set_image("tex", 2, 2, vec![128u8; 16]).unwrap();
    let layers = engine.resolved_layers().unwrap();
    assert_eq!(layers.len(), 2, "missing image still skipped");
    assert_eq!(
        layers[0].shape,
        ResolvedShape::Image {
            image: "tex".into(),
            x: 4.0,
            y: 6.0,
            width: 2.0,
            height: 2.0
        }
    );
    match &layers[1].shape {
        ResolvedShape::Image { width, height, .. } => {
            assert_eq!((*width, *height), (10.0, 20.0));
        }
        other => panic!("expected image, got {other:?}"),
    }
    let data = engine.image("tex").unwrap();
    assert_eq!((data.width, data.height, data.pixels.len()), (2, 2, 16));

    // Re-registering bumps the revision, letting renderers cache uploads.
    let first = data.revision();
    engine.set_image("tex", 2, 2, vec![64u8; 16]).unwrap();
    assert!(engine.image("tex").unwrap().revision() > first);
}

#[test]
fn autoplay_looping_timeline_runs_and_wraps() {
    const SCENE: &str = r##"{
        "name": "looper",
        "size": [8, 8],
        "layers": [
            {
                "name": "dot",
                "type": "shape",
                "shape": { "circle": [0, 0, 1] },
                "fill": "#FFFFFF",
                "timelines": [
                    {
                        "name": "cycle",
                        "autoplay": true,
                        "loop": true,
                        "tracks": [
                            { "property": "x", "keys": [
                                { "t": 0.0, "v": 0.0 },
                                { "t": 2.0, "v": 2.0 }
                            ] }
                        ]
                    }
                ]
            }
        ]
    }"##;
    let dot_x = |e: &Engine| match e.resolved_layers().unwrap()[0].shape {
        ResolvedShape::Circle { cx, .. } => cx,
        _ => panic!("dot should be a circle"),
    };
    let mut engine = Engine::new();
    engine.load_show(SCENE).unwrap();
    // Started at load, no trigger required.
    assert_eq!(dot_x(&engine), 0.0);
    engine.advance_frame(1.0);
    assert!((dot_x(&engine) - 1.0).abs() < 1e-9);
    // 2.5s in: wrapped around to 0.5s.
    engine.advance_frame(1.5);
    assert!((dot_x(&engine) - 0.5).abs() < 1e-9);
}

#[test]
fn scale_property_binds_and_scales_geometry() {
    const SCENE: &str = r##"{
        "name": "scaled",
        "size": [100, 100],
        "variables": { "level": 0 },
        "layers": [
            {
                "name": "halo",
                "type": "shape",
                "shape": { "circle": [0, 0, 10] },
                "fill": "#FFFFFF",
                "x": 50,
                "y": 50,
                "bindings": [
                    { "property": "scale", "variable": "level", "scale": 2.0, "offset": 0.5 }
                ]
            }
        ]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(SCENE).unwrap();
    let halo = |e: &Engine| match e.resolved_layers().unwrap()[0].shape {
        ResolvedShape::Circle { cx, cy, radius } => (cx, cy, radius),
        _ => panic!("halo should be a circle"),
    };
    // level 0: scale = 0.5, radius halves, position (the layer origin) stays.
    assert_eq!(halo(&engine), (50.0, 50.0, 5.0));
    engine.set_variable("level", 1.0);
    // level 1: scale = 2.5.
    assert_eq!(halo(&engine), (50.0, 50.0, 25.0));
}

#[test]
fn unknown_variable_and_trigger_are_harmless() {
    let mut engine = Engine::new();
    engine.load_show(MINIGOLF).unwrap();
    engine.set_variable("nonexistent", true);
    engine.trigger("nonexistent");
    engine.advance_frame(0.1);
    assert_eq!(engine.resolved_layers().unwrap().len(), 6);
}

const SCENES: &str = r##"{
  "name": "scenes",
  "size": [64, 32],
  "layers": [
    { "name": "frame", "type": "shape", "shape": { "rect": [0, 0, 64, 1] }, "fill": "#FFFFFF" }
  ],
  "scenes": [
    {
      "name": "attract",
      "trigger": "attract",
      "layers": [
        {
          "name": "logo",
          "type": "shape",
          "shape": { "circle": [0, 0, 4] },
          "fill": "#FF0000",
          "timelines": [
            {
              "name": "slide",
              "autoplay": true,
              "tracks": [{ "property": "x", "keys": [{ "t": 0, "v": 0 }, { "t": 1, "v": 10 }] }]
            }
          ]
        }
      ]
    },
    {
      "name": "game",
      "trigger": "start",
      "layers": [
        {
          "name": "score",
          "type": "shape",
          "shape": { "rect": [0, 0, 8, 8] },
          "fill": "#00FF00",
          "timelines": [
            {
              "name": "pop",
              "trigger": "start",
              "tracks": [{ "property": "scale", "keys": [{ "t": 0, "v": 2 }, { "t": 1, "v": 1 }] }]
            }
          ]
        }
      ]
    }
  ]
}"##;

fn names(engine: &Engine) -> Vec<String> {
    engine
        .resolved_layers()
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect()
}

fn circle_cx(engine: &Engine, name: &str) -> f64 {
    let layers = engine.resolved_layers().unwrap();
    match layers.iter().find(|l| l.name == name).unwrap().shape {
        ResolvedShape::Circle { cx, .. } => cx,
        _ => panic!("{name} should be a circle"),
    }
}

#[test]
fn first_scene_is_active_and_show_layers_paint_behind_it() {
    let mut engine = Engine::new();
    engine.load_show(SCENES).unwrap();
    assert_eq!(engine.active_scene(), Some("attract"));
    assert_eq!(names(&engine), ["frame", "logo"]);
}

#[test]
fn trigger_enters_scene_and_starts_its_timelines() {
    let mut engine = Engine::new();
    engine.load_show(SCENES).unwrap();
    engine.trigger("start");
    assert_eq!(engine.active_scene(), Some("game"));
    assert_eq!(names(&engine), ["frame", "score"]);
    // The entering trigger also fires the scene's own timelines.
    let width = |e: &Engine| match e.resolved_layers().unwrap()[1].shape {
        ResolvedShape::Rect { width, .. } => width,
        _ => panic!("score should be a rect"),
    };
    assert_eq!(width(&engine), 16.0);
    engine.advance_frame(0.5);
    assert_eq!(width(&engine), 12.0);
}

#[test]
fn reentering_a_scene_restarts_its_autoplay_timelines() {
    let mut engine = Engine::new();
    engine.load_show(SCENES).unwrap();
    engine.advance_frame(0.5);
    assert_eq!(circle_cx(&engine, "logo"), 5.0);
    engine.trigger("start");
    engine.advance_frame(0.2);
    engine.trigger("attract");
    assert_eq!(circle_cx(&engine, "logo"), 0.0);
    engine.advance_frame(0.25);
    assert_eq!(circle_cx(&engine, "logo"), 2.5);
    // Firing the active scene's trigger restarts it too.
    engine.trigger("attract");
    assert_eq!(circle_cx(&engine, "logo"), 0.0);
}

#[test]
fn inactive_scene_timelines_do_not_run() {
    let mut engine = Engine::new();
    engine.load_show(SCENES).unwrap();
    // "pop" lives in the inactive game scene: firing a timeline-only
    // trigger there must not leave a playhead behind.
    engine.trigger("attract");
    engine.advance_frame(2.0);
    assert_eq!(engine.active_scene(), Some("attract"));
    assert_eq!(names(&engine), ["frame", "logo"]);
}

#[test]
fn show_without_scenes_has_no_active_scene() {
    let mut engine = Engine::new();
    engine.load_show(MINIGOLF).unwrap();
    assert_eq!(engine.active_scene(), None);
}

const ANCHORS: &str = r##"{
  "name": "anchors",
  "size": [64, 64],
  "layers": [
    { "name": "img", "type": "image", "image": "px", "size": [8, 4], "x": 32, "y": 32, "anchor": "bottom_right" },
    { "name": "grow", "type": "image", "image": "px", "size": [8, 8], "x": 32, "y": 32, "anchor": "center", "scale": 2 },
    { "name": "bar", "type": "shape", "shape": { "rect": [0, 0, 10, 2] }, "fill": "#FFFFFF", "x": 64, "anchor": "top_right" },
    { "name": "dot", "type": "shape", "shape": { "circle": [0, 0, 3] }, "fill": "#FFFFFF", "x": 10, "y": 10, "anchor": "top_left" },
    { "name": "plain", "type": "shape", "shape": { "circle": [0, 0, 3] }, "fill": "#FFFFFF", "x": 10, "y": 10 }
  ]
}"##;

#[test]
fn anchor_places_content_box_point_at_position() {
    let mut engine = Engine::new();
    engine.set_image("px", 1, 1, vec![255u8; 4]).unwrap();
    engine.load_show(ANCHORS).unwrap();
    let layers = engine.resolved_layers().unwrap();
    let shape = |name: &str| {
        layers
            .iter()
            .find(|l| l.name == name)
            .unwrap()
            .shape
            .clone()
    };
    let image = |name: &str| match shape(name) {
        ResolvedShape::Image {
            x,
            y,
            width,
            height,
            ..
        } => (x, y, width, height),
        other => panic!("{other:?}"),
    };
    assert_eq!(image("img"), (24.0, 28.0, 8.0, 4.0));
    // Scaling keeps the anchored center in place.
    assert_eq!(image("grow"), (24.0, 24.0, 16.0, 16.0));
    assert_eq!(
        shape("bar"),
        ResolvedShape::Rect {
            x: 54.0,
            y: 0.0,
            width: 10.0,
            height: 2.0
        }
    );
    assert_eq!(
        shape("dot"),
        ResolvedShape::Circle {
            cx: 13.0,
            cy: 13.0,
            radius: 3.0
        }
    );
    // No anchor: shapes keep their local origin at x/y.
    assert_eq!(
        shape("plain"),
        ResolvedShape::Circle {
            cx: 10.0,
            cy: 10.0,
            radius: 3.0
        }
    );
}

#[test]
fn anchor_on_group_is_rejected() {
    let mut engine = Engine::new();
    let show = r#"{ "name": "g", "size": [8, 8], "layers": [
        { "name": "g", "type": "group", "children": [], "anchor": "center" } ] }"#;
    assert!(engine.load_show(show).is_err());
}

#[test]
fn clipped_group_brackets_its_children() {
    let show = r##"{ "name": "clip", "size": [64, 32], "layers": [
        { "name": "content", "type": "group", "x": 40, "clip": { "rect": [0, 0, 20, 32] }, "children": [
            { "name": "title", "type": "shape", "shape": { "rect": [0, 0, 100, 8] }, "fill": "#FFFFFF" }
        ] },
        { "name": "after", "type": "shape", "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF" }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let shapes: Vec<_> = engine
        .resolved_layers()
        .unwrap()
        .into_iter()
        .map(|l| l.shape)
        .collect();
    assert_eq!(
        shapes[0],
        ResolvedShape::ClipBegin {
            shape: Box::new(ResolvedShape::Rect {
                x: 40.0,
                y: 0.0,
                width: 20.0,
                height: 32.0
            })
        }
    );
    assert!(matches!(shapes[1], ResolvedShape::Rect { x, .. } if x == 40.0));
    assert_eq!(shapes[2], ResolvedShape::ClipEnd);
    assert_eq!(shapes.len(), 4);
}
