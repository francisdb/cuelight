//! Structural tests: the whole trigger/variable/timeline model with no GPU.

use cuelight::{Engine, Event, ResolvedShape};

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
            source: None,
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

const SHEET: &str = r##"{
  "name": "sheet",
  "size": [32, 32],
  "layers": [
    {
      "name": "girl",
      "type": "image",
      "image": "sheet",
      "sheet": { "cell": [4, 2], "columns": 3 },
      "frame": 1,
      "x": 5,
      "timelines": [
        {
          "name": "run",
          "trigger": "run",
          "loop": true,
          "tracks": [{ "property": "frame", "keys": [{ "t": 0, "v": 0 }, { "t": 0.6, "v": 6 }] }]
        }
      ]
    }
  ]
}"##;

fn source(engine: &Engine) -> (Option<[u32; 4]>, f64, f64) {
    match engine.resolved_layers().unwrap()[0].shape.clone() {
        ResolvedShape::Image {
            source,
            width,
            height,
            ..
        } => (source, width, height),
        other => panic!("{other:?}"),
    }
}

#[test]
fn sheet_draws_the_frame_cell() {
    let mut engine = Engine::new();
    // 12x4 image: 3 columns x 2 rows of 4x2 cells.
    engine
        .set_image("sheet", 12, 4, vec![255u8; 12 * 4 * 4])
        .unwrap();
    engine.load_show(SHEET).unwrap();
    // Base frame 1, drawn at the cell's size.
    assert_eq!(source(&engine), (Some([4, 0, 4, 2]), 4.0, 2.0));
    engine.trigger("run");
    engine.advance_frame(0.35);
    // Frame 3.5 rounds down to 3: first cell of the second row.
    assert_eq!(source(&engine).0, Some([0, 2, 4, 2]));
    engine.set_variable("unused", 0.0);
    engine.advance_frame(0.2);
    // 5.5 -> 5, the last cell.
    assert_eq!(source(&engine).0, Some([8, 2, 4, 2]));
}

#[test]
fn sheet_frame_is_clamped_and_bindable() {
    let show = SHEET.replace(
        r#""frame": 1,"#,
        r#""frame": 1, "bindings": [{ "property": "frame", "variable": "f" }],"#,
    );
    let mut engine = Engine::new();
    engine
        .set_image("sheet", 12, 4, vec![255u8; 12 * 4 * 4])
        .unwrap();
    engine.load_show(&show).unwrap();
    engine.set_variable("f", 99.0);
    assert_eq!(source(&engine).0, Some([8, 2, 4, 2]));
    engine.set_variable("f", -3.0);
    assert_eq!(source(&engine).0, Some([0, 0, 4, 2]));
}

#[test]
fn sheet_anchor_uses_the_cell_size_and_frame_needs_an_image() {
    let show = SHEET.replace(r#""x": 5,"#, r#""x": 16, "y": 16, "anchor": "center","#);
    let mut engine = Engine::new();
    engine
        .set_image("sheet", 12, 4, vec![255u8; 12 * 4 * 4])
        .unwrap();
    engine.load_show(&show).unwrap();
    match engine.resolved_layers().unwrap()[0].shape {
        ResolvedShape::Image { x, y, .. } => assert_eq!((x, y), (14.0, 15.0)),
        ref other => panic!("{other:?}"),
    }
    // frame only exists on image layers
    let show = r##"{ "name": "s", "size": [8, 8], "layers": [
        { "name": "box", "type": "shape", "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF",
          "bindings": [{ "property": "frame", "variable": "f" }] } ] }"##;
    assert!(engine.load_show(show).is_err());
}

const PLAYBACK: &str = r##"{
  "name": "playback",
  "size": [64, 64],
  "layers": [
    {
      "name": "scroll",
      "type": "shape",
      "shape": { "rect": [0, 0, 1, 1] },
      "fill": "#FFFFFF",
      "x": 5,
      "timelines": [
        {
          "name": "scroll",
          "autoplay": true,
          "loop": true,
          "delay": 2.0,
          "tracks": [{ "property": "x", "keys": [{ "t": 0, "v": 0 }, { "t": 10, "v": -10 }] }]
        }
      ]
    },
    {
      "name": "blink",
      "type": "shape",
      "shape": { "rect": [0, 0, 1, 1] },
      "fill": "#FFFFFF",
      "timelines": [
        {
          "name": "cycle",
          "trigger": "go",
          "repeat": 2.5,
          "on_end": "done",
          "tracks": [{ "property": "x", "keys": [{ "t": 0, "v": 0 }, { "t": 1, "v": 10 }] }]
        },
        {
          "name": "after",
          "trigger": "done",
          "tracks": [{ "property": "y", "keys": [{ "t": 0, "v": 7 }, { "t": 1, "v": 7 }] }]
        }
      ]
    }
  ]
}"##;

fn rect_xy(engine: &Engine, name: &str) -> (f64, f64) {
    let layers = engine.resolved_layers().unwrap();
    match layers.iter().find(|l| l.name == name).unwrap().shape {
        ResolvedShape::Rect { x, y, .. } => (x, y),
        ref other => panic!("{other:?}"),
    }
}

#[test]
fn delay_holds_the_base_value_and_is_not_looped() {
    let mut engine = Engine::new();
    engine.load_show(PLAYBACK).unwrap();
    engine.advance_frame(1.5);
    // Still delayed: the base x applies.
    assert_eq!(rect_xy(&engine, "scroll").0, 5.0);
    engine.advance_frame(1.5);
    assert_eq!(rect_xy(&engine, "scroll").0, -1.0);
    // 2s delay + 10s play: wraps to the start without waiting again.
    engine.advance_frame(9.0);
    assert_eq!(rect_xy(&engine, "scroll").0, 0.0);
    engine.advance_frame(1.0);
    assert_eq!(rect_xy(&engine, "scroll").0, -1.0);
}

#[test]
fn repeat_plays_n_times_then_fires_on_end() {
    let mut engine = Engine::new();
    engine.load_show(PLAYBACK).unwrap();
    engine.trigger("go");
    engine.advance_frame(1.25);
    // Second play, a quarter in.
    assert_eq!(rect_xy(&engine, "blink"), (2.5, 0.0));
    engine.advance_frame(1.0);
    assert_eq!(rect_xy(&engine, "blink"), (2.5, 0.0));
    assert!(engine.drain_events().is_empty());
    // 2.5 plays end at 2.5s: on_end fires "done", which starts "after"
    // and is reported to the host once.
    engine.advance_frame(0.5);
    assert_eq!(rect_xy(&engine, "blink"), (0.0, 7.0));
    assert_eq!(engine.drain_events(), [Event::Trigger("done".into())]);
    assert!(engine.drain_events().is_empty());
    engine.advance_frame(1.0);
    assert_eq!(rect_xy(&engine, "blink"), (0.0, 0.0));
}

#[test]
fn loop_with_repeat_is_rejected() {
    let mut engine = Engine::new();
    let show = PLAYBACK.replace(r#""delay": 2.0,"#, r#""delay": 2.0, "repeat": 2,"#);
    assert!(engine.load_show(&show).is_err());
}

#[test]
fn on_end_can_enter_a_scene() {
    let show = r##"{ "name": "s", "size": [8, 8], "scenes": [
        { "name": "intro", "trigger": "intro", "layers": [
            { "name": "logo", "type": "shape", "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF",
              "timelines": [{ "name": "hold", "autoplay": true, "on_end": "menu",
                "tracks": [{ "property": "x", "keys": [{ "t": 0, "v": 0 }, { "t": 1, "v": 1 }] }] }] } ] },
        { "name": "menu", "trigger": "menu", "layers": [] }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine.advance_frame(0.5);
    assert_eq!(engine.active_scene(), Some("intro"));
    engine.advance_frame(0.6);
    assert_eq!(engine.active_scene(), Some("menu"));
    assert_eq!(engine.drain_events(), [Event::Trigger("menu".into())]);
}

const DIGITS: &str = r##"{ "name": "d", "size": [32, 16], "variables": { "score": 1 }, "layers": [
    { "name": "d", "type": "digits", "digits": 2, "size": [16, 16], "x": 4, "justify": "right",
      "display": { "segments": { "style": "numeric7", "fill": "#FF0000", "unlit": "#200000" } },
      "bindings": [{ "property": "text", "variable": "score" }] }
] }"##;

#[test]
fn digit_row_draws_lit_and_unlit_segments() {
    let mut engine = Engine::new();
    engine.load_show(DIGITS).unwrap();
    let layers = engine.resolved_layers().unwrap();
    let lit: Vec<_> = layers
        .iter()
        .filter(|l| l.color == [255, 0, 0, 255])
        .collect();
    let unlit = layers.iter().filter(|l| l.color == [32, 0, 0, 255]).count();
    // "1" right-justified: b and c of the second cell; everything else in
    // both cells (7 segments + dot each) is drawn unlit.
    assert_eq!(lit.len(), 2);
    assert_eq!(unlit, 8 + 6);
    for l in lit {
        let ResolvedShape::Polygon { points } = &l.shape else {
            panic!("segments resolve to polygons");
        };
        // second 8px cell of a row starting at x = 4: its right side
        assert!(
            points.iter().all(|&[x, _]| (17.0..20.0).contains(&x)),
            "{points:?}"
        );
    }
}

#[test]
fn digit_row_text_is_bindable() {
    let mut engine = Engine::new();
    engine.load_show(DIGITS).unwrap();
    let lit = |e: &Engine| {
        let layers = e.resolved_layers().unwrap();
        layers
            .iter()
            .filter(|l| l.color == [255, 0, 0, 255])
            .count()
    };
    assert_eq!(lit(&engine), 2);
    // "38": 5 + 7 segments
    engine.set_variable("score", 38.0);
    assert_eq!(lit(&engine), 12);
}

#[test]
fn timelines_and_scenes_can_listen_to_several_triggers() {
    let show = r##"{ "name": "s", "size": [8, 8], "scenes": [
        { "name": "idle", "trigger": ["idle", "reset"], "layers": [] },
        { "name": "signals", "trigger": "signals", "layers": [
            { "name": "left", "type": "shape", "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF",
              "timelines": [{ "name": "blink", "trigger": ["turn_left", "hazard"],
                "tracks": [{ "property": "x", "keys": [{ "t": 0, "v": 5 }, { "t": 1, "v": 5 }] }] }] } ] }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    assert!(engine.load_warnings().is_empty());
    engine.trigger("signals");
    let x = |e: &Engine| match e.resolved_layers().unwrap()[0].shape {
        ResolvedShape::Rect { x, .. } => x,
        ref other => panic!("{other:?}"),
    };
    for name in ["turn_left", "hazard"] {
        assert_eq!(x(&engine), 0.0);
        engine.trigger(name);
        assert_eq!(x(&engine), 5.0, "{name}");
        engine.advance_frame(1.5);
    }
    engine.trigger("turn_right");
    assert_eq!(x(&engine), 0.0);
    // either name enters the scene
    engine.trigger("reset");
    assert_eq!(engine.active_scene(), Some("idle"));
}

#[test]
fn triggers_serialize_the_way_they_are_authored() {
    use cuelight::Triggers;
    let json = |t: &Triggers| serde_json::to_string(t).unwrap();
    assert_eq!(json(&Triggers(vec![])), "null");
    assert_eq!(json(&Triggers(vec!["go".into()])), r#""go""#);
    assert_eq!(
        json(&Triggers(vec!["a".into(), "b".into()])),
        r#"["a","b"]"#
    );
    let back: Triggers = serde_json::from_str(r#"["a","b"]"#).unwrap();
    assert!(back.contains("b") && !back.contains("c"));
}

#[test]
fn a_show_lists_its_triggers() {
    let show = r##"{ "name": "s", "size": [8, 8],
      "layers": [ { "name": "g", "type": "group", "children": [
          { "name": "a", "type": "shape", "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF",
            "timelines": [ { "name": "t", "trigger": ["flash", "hazard"], "tracks": [] },
                           { "name": "auto", "autoplay": true, "tracks": [] } ] } ] } ],
      "scenes": [ { "name": "one", "trigger": "start", "layers": [
          { "name": "b", "type": "shape", "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF",
            "timelines": [ { "name": "t", "trigger": "flash", "tracks": [] } ] } ] } ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let triggers: Vec<String> = engine.show().unwrap().triggers().into_iter().collect();
    assert_eq!(triggers, ["flash", "hazard", "start"]);
}

#[test]
fn blend_is_carried_by_the_draw_list() {
    use cuelight::Blend;
    let show = r##"{ "name": "blend", "size": [8, 8], "layers": [
        { "name": "lamp", "type": "shape", "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF", "blend": "add" },
        { "name": "glow", "type": "group", "blend": "screen", "clip": { "rect": [0, 0, 4, 4] }, "children": [
            { "name": "in", "type": "shape", "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF" }
        ] }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let layers = engine.resolved_layers().unwrap();
    assert_eq!(layers[0].blend, Blend::Add);
    // The blend opens before the clip and closes after it.
    let shapes: Vec<String> = layers[1..]
        .iter()
        .map(|l| {
            format!("{:?}", l.shape)
                .split([' ', '{'])
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        shapes,
        ["BlendBegin", "ClipBegin", "Rect", "ClipEnd", "BlendEnd"]
    );
    assert_eq!(
        layers[1].shape,
        ResolvedShape::BlendBegin {
            blend: Blend::Screen
        }
    );
    assert_eq!(layers[3].blend, Blend::Normal);
}

#[test]
fn a_shape_can_be_filled_with_a_gradient() {
    let show = r##"{
      "name": "glow", "size": [64, 64],
      "layers": [
        { "name": "solid", "type": "shape", "shape": { "rect": [0, 0, 8, 8] },
          "fill": "#FF0000" },
        { "name": "glow", "type": "shape", "x": 10, "y": 20, "scale": 2,
          "shape": { "circle": [0, 0, 30] },
          "fill": { "radial": { "center": [0, 0], "radius": 30,
                                "stops": [{ "at": 0, "color": "#FFFFFFFF" },
                                          { "at": 1, "color": "#FFFFFF00" }] } } }
      ]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let layers = engine.resolved_layers().unwrap();

    let solid = layers.iter().find(|l| l.name == "solid").unwrap();
    assert_eq!(solid.color, [255, 0, 0, 255]);
    assert!(solid.gradient.is_none(), "a color is still a color");

    let glow = layers.iter().find(|l| l.name == "glow").unwrap();
    let gradient = glow.gradient.as_ref().unwrap();
    // Placed and scaled with the shape, so it travels with it.
    assert_eq!(
        gradient.kind,
        cuelight::ResolvedGradientKind::Radial {
            center: [10.0, 20.0],
            radius: 60.0
        }
    );
    assert_eq!(
        gradient.stops,
        [(0.0, [255, 255, 255, 255]), (1.0, [255, 255, 255, 0])]
    );
    // A host that draws no gradients still has something to draw.
    assert_eq!(glow.color, [255, 255, 255, 255]);
}

#[test]
fn a_gradient_needs_stops_in_order_and_a_radius() {
    let bad = |fill: &str| {
        format!(
            r##"{{ "name": "g", "size": [8, 8], "layers": [
                 {{ "name": "s", "type": "shape", "shape": {{ "rect": [0, 0, 8, 8] }},
                    "fill": {fill} }}] }}"##
        )
    };
    let stops = r##""stops": [{ "at": 0, "color": "#FFFFFF" }]"##;
    assert!(Engine::new()
        .load_show(&bad(&format!(
            r#"{{ "radial": {{ "center": [0, 0], "radius": 0, {stops} }} }}"#
        )))
        .is_err());
    assert!(Engine::new()
        .load_show(&bad(r##"{ "linear": { "from": [0, 0], "to": [0, 8],
                   "stops": [{ "at": 1, "color": "#FFFFFF" },
                             { "at": 0, "color": "#000000" }] } }"##))
        .is_err());
    assert!(Engine::new()
        .load_show(&bad(r##"{ "linear": { "from": [0, 0], "to": [0, 8],
                   "stops": [{ "at": 0, "color": "purple" }] } }"##))
        .is_err());
}
