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
            height: 2.0,
            tile: None
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
          "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF",
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
fn only_lit_segments_glow() {
    let show = DIGITS.replace(
        r##""unlit": "#200000""##,
        r##""unlit": "#200000", "glow": { "size": 0.2, "strength": 1.0 }"##,
    );
    let mut engine = Engine::new();
    engine.load_show(&show).unwrap();
    // Two cells of seven segments and a dot, lit or unlit.
    const CELLS: usize = 16;
    let halo = |e: &Engine, lit: usize| {
        let layers = e.resolved_layers().unwrap();
        // The unlit segments are drawn once each and never glow, and
        // nothing is drawn in a colour that is neither.
        assert_eq!(
            layers.iter().filter(|l| l.color == [32, 0, 0, 255]).count(),
            CELLS - lit
        );
        let halo: Vec<_> = layers
            .iter()
            .filter(|l| l.color[..3] == [255, 0, 0] && l.color[3] < 255)
            .collect();
        assert_eq!(
            layers.len() - halo.len(),
            CELLS,
            "one pass of lit and unlit segments under the halo"
        );
        // Every pass covers each lit segment once, so the halo is a whole
        // number of passes over the lit ones and nothing else.
        assert_eq!(halo.len() % lit, 0, "{} over {lit} lit", halo.len());
        halo.len() / lit
    };
    // "1": b and c. Then "38": 5 + 7 segments, same number of passes.
    let passes = halo(&engine, 2);
    assert!(passes > 1, "a halo of {passes} passes is a single outline");
    engine.set_variable("score", 38.0);
    assert_eq!(halo(&engine, 12), passes);
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

/// One layer, one fade in, optionally held, plus a flash that can take
/// the property for a moment.
fn fading(hold: bool) -> Engine {
    let show = format!(
        r##"{{
      "name": "fade", "size": [8, 8],
      "layers": [{{
        "name": "panel", "type": "shape", "opacity": 0,
        "shape": {{ "rect": [0, 0, 8, 8] }}, "fill": "#FFFFFF",
        "timelines": [
          {{ "name": "in", "trigger": "enter", "hold": {hold},
             "tracks": [{{ "property": "opacity",
                          "keys": [{{ "t": 0, "v": 0 }}, {{ "t": 1, "v": 0.75 }}] }}] }},
          {{ "name": "flash", "trigger": "flash",
             "tracks": [{{ "property": "opacity",
                          "keys": [{{ "t": 0, "v": 1 }}, {{ "t": 0.5, "v": 1 }}] }}] }}
        ]
      }}]
    }}"##
    );
    let mut engine = Engine::new();
    engine.load_show(&show).unwrap();
    engine
}

fn opacity(engine: &Engine) -> f64 {
    engine.resolved_layers().unwrap()[0].opacity
}

#[test]
fn a_timeline_gives_its_property_back_when_it_ends() {
    let mut engine = fading(false);
    engine.trigger("enter");
    engine.advance_frame(0.5);
    assert!(
        (opacity(&engine) - 0.375).abs() < 0.01,
        "{}",
        opacity(&engine)
    );
    engine.advance_frame(1.0);
    assert_eq!(opacity(&engine), 0.0, "back to the layer's own value");
}

#[test]
fn a_held_timeline_keeps_its_last_value() {
    let mut engine = fading(true);
    engine.trigger("enter");
    engine.advance_frame(1.5);
    assert!(
        (opacity(&engine) - 0.75).abs() < 0.001,
        "{}",
        opacity(&engine)
    );
    // And keeps it, rather than being a value that decays.
    engine.advance_frame(10.0);
    assert!(
        (opacity(&engine) - 0.75).abs() < 0.001,
        "{}",
        opacity(&engine)
    );
}

#[test]
fn a_held_timeline_of_one_key_sets_its_value_and_keeps_it() {
    // "Set this and keep it", from a trigger: the shortest way to write
    // a state change, and it has no duration at all.
    let show = r##"{
      "name": "zero", "size": [8, 8],
      "layers": [{
        "name": "caption", "type": "shape", "opacity": 0,
        "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF",
        "timelines": [
          { "name": "on", "trigger": "show", "hold": true, "on_end": "shown",
            "tracks": [{ "property": "opacity", "keys": [{ "t": 0, "v": 1 }] }] },
          { "name": "off", "trigger": "hide", "hold": true,
            "tracks": [{ "property": "opacity", "keys": [{ "t": 0, "v": 0 }] }] }
        ]
      }]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine.advance_to(0.5);
    engine.trigger("show");
    engine.advance_to(1.0);
    assert_eq!(opacity(&engine), 1.0, "applied");
    engine.advance_to(2.0);
    assert_eq!(opacity(&engine), 1.0, "and kept");
    // It ends the instant it starts, and says so once like any other.
    assert!(engine
        .drain_events()
        .iter()
        .any(|e| matches!(e, Event::Trigger(t) if t == "shown")));
    engine.advance_to(3.0);
    assert!(engine.drain_events().is_empty(), "once, not every frame");
    // And another of the same shape takes it back.
    engine.trigger("hide");
    engine.advance_to(3.5);
    assert_eq!(opacity(&engine), 0.0);
}

#[test]
fn a_timeline_of_one_key_without_hold_gives_the_property_back() {
    let show = r##"{
      "name": "zero", "size": [8, 8],
      "layers": [{
        "name": "caption", "type": "shape", "opacity": 0.25,
        "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF",
        "timelines": [{ "name": "on", "trigger": "show",
                        "tracks": [{ "property": "opacity",
                                     "keys": [{ "t": 0, "v": 1 }] }] }]
      }]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine.trigger("show");
    engine.advance_to(1.0);
    assert_eq!(opacity(&engine), 0.25, "nothing to hold it");
}

#[test]
fn a_running_timeline_outranks_a_held_one_and_hands_back() {
    let mut engine = fading(true);
    engine.trigger("enter");
    engine.advance_frame(1.5);
    engine.trigger("flash");
    engine.advance_frame(0.1);
    assert!((opacity(&engine) - 1.0).abs() < 0.001, "the flash wins");
    engine.advance_frame(1.0);
    assert!(
        (opacity(&engine) - 0.75).abs() < 0.001,
        "and hands back to the held value, not to 0: {}",
        opacity(&engine)
    );
}

#[test]
fn a_held_timeline_still_fires_its_end_and_can_be_restarted() {
    let show = r##"{
      "name": "held", "size": [8, 8],
      "layers": [{
        "name": "panel", "type": "shape", "opacity": 0,
        "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF",
        "timelines": [{ "name": "in", "trigger": "enter", "hold": true, "on_end": "done",
                        "tracks": [{ "property": "opacity",
                                     "keys": [{ "t": 0, "v": 0 }, { "t": 1, "v": 0.75 }] }] }]
      }]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine.trigger("enter");
    engine.advance_frame(1.5);
    assert!(engine
        .drain_events()
        .iter()
        .any(|e| matches!(e, Event::Trigger(t) if t == "done")));
    engine.advance_frame(1.0);
    assert!(
        engine.drain_events().is_empty(),
        "it ends once, not every frame"
    );

    // Started again, it plays again from the top.
    engine.trigger("enter");
    engine.advance_frame(0.0);
    assert!(opacity(&engine) < 0.01, "{}", opacity(&engine));
}

#[test]
fn restarting_reaches_the_same_state_as_a_fresh_load() {
    let mut walked = Engine::new();
    walked.load_show(MINIGOLF).unwrap();
    walked.trigger("putt");
    for _ in 0..120 {
        walked.advance_frame(1.0 / 60.0);
    }
    walked.set_variable("score", 7.0);

    // Put back, then walked to the same place again.
    walked.restart();
    assert_eq!(walked.time(), 0.0);
    let mut fresh = Engine::new();
    fresh.load_show(MINIGOLF).unwrap();

    for _ in 0..30 {
        walked.advance_frame(1.0 / 60.0);
        fresh.advance_frame(1.0 / 60.0);
    }
    assert_eq!(
        walked.resolved_layers().unwrap(),
        fresh.resolved_layers().unwrap(),
        "a restart is the start, however the engine got there"
    );
    // Variables go back to what the document declares, not what a host
    // last set: a restart is the show's beginning, not the host's.
    assert_eq!(walked.variable("score"), fresh.variable("score"));
}

#[test]
fn restarting_without_a_show_does_nothing() {
    let mut engine = Engine::new();
    engine.restart();
    assert_eq!(engine.time(), 0.0);
}

/// Three ten-second clips, each starting the next: the show is thirty
/// seconds long whatever the frame rate, including one frame every ten
/// seconds.
#[test]
fn a_chain_lasts_what_its_parts_add_up_to_at_any_frame_rate() {
    let show = r#"{
      "name": "chain", "size": [8, 8],
      "layers": [{ "name": "screen", "type": "video", "video": "a",
                   "autoplay": true, "on_end": "b" },
                 { "name": "screen2", "type": "video", "video": "b",
                   "trigger": "b", "on_end": "c" },
                 { "name": "screen3", "type": "video", "video": "c",
                   "trigger": "c", "on_end": "done" }]
    }"#;
    for fps in [60.0, 30.0, 7.0, 3.0, 1.0, 0.1] {
        let mut engine = Engine::new();
        for name in ["a", "b", "c"] {
            engine.set_video(name, 10.0, [8.0, 8.0]).unwrap();
        }
        engine.load_show(show).unwrap();

        let step = 1.0 / fps;
        let mut done = None;
        let mut t = 0.0;
        while t < 40.0 && done.is_none() {
            engine.advance_frame(step);
            t += step;
            if engine
                .drain_events()
                .iter()
                .any(|e| matches!(e, Event::Trigger(name) if name == "done"))
            {
                // The frame it was noticed in; the show's own clock is
                // what has to be exact.
                done = Some(engine.time());
            }
        }
        let at = done.unwrap_or_else(|| panic!("{fps} fps: never finished"));
        // Noticed in the frame the thirtieth second falls in: never
        // before it, and never a frame after it. The clock itself is a
        // running total of the frame lengths the host handed in, so it
        // is a hair off a round thirty by the time it gets there.
        assert!(
            at > 30.0 - 1e-6 && at < 30.0 + step,
            "{fps} fps: finished at {at}, wanted 30.0 within one frame"
        );
    }
}

#[test]
fn a_chain_of_timelines_keeps_its_spacing_however_the_frames_fall() {
    // 0.7 s links against a sixtieth of a second: neither is exact in
    // binary, so a link's end sits a hair off a frame boundary, on
    // whichever side the rounding fell. Two timelines firing each other
    // round and round, so any slack in the handover would pile up.
    let show = r##"{
      "format": 1, "name": "chain", "size": [8, 8],
      "layers": [{ "name": "box", "type": "shape",
                   "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF",
                   "timelines": [
        { "name": "a", "trigger": ["go", "d2"], "on_end": "d1", "hold": true,
          "tracks": [{ "property": "x", "keys": [{"t":0,"v":0},{"t":0.7,"v":1}] }] },
        { "name": "b", "trigger": "d1", "on_end": "d2", "hold": true,
          "tracks": [{ "property": "y", "keys": [{"t":0,"v":0},{"t":0.7,"v":1}] }] }
      ]}]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine.trigger("go");

    let mut ends = Vec::new();
    for frame in 1..=4200 {
        engine.advance_frame(1.0 / 60.0);
        for event in engine.drain_events() {
            if matches!(&event, Event::Trigger(name) if name.starts_with('d')) {
                ends.push(frame);
            }
        }
    }
    // 0.7 s is 42 frames, a hundred times over, the last at 70 seconds.
    let want: Vec<usize> = (1..=100).map(|n| n * 42).collect();
    assert_eq!(ends, want);
}

#[test]
fn what_starts_late_is_already_that_far_into_itself() {
    // A 10.5 s clip at 1 fps ends half way through the eleventh frame.
    // The timeline its `on_end` starts is not seen until that frame is
    // over, half a second later, and by then it is half a second in.
    let show = r##"{
      "format": 1, "name": "late", "size": [8, 8],
      "layers": [
        { "name": "screen", "type": "video", "video": "clip",
          "autoplay": true, "on_end": "next" },
        { "name": "box", "type": "shape", "shape": { "rect": [0, 0, 1, 1] },
          "fill": "#FFFFFF", "x": 0,
          "timelines": [{ "name": "slide", "trigger": "next",
            "tracks": [{ "property": "x",
                         "keys": [{"t":0,"v":0},{"t":10,"v":100}] }] }] }
      ]
    }"##;
    let mut engine = Engine::new();
    engine.set_video("clip", 10.5, [8.0, 8.0]).unwrap();
    engine.load_show(show).unwrap();

    for _ in 0..11 {
        engine.advance_frame(1.0);
    }
    let layers = engine.resolved_layers().unwrap();
    let box_x = layers.iter().find(|l| l.name == "box").unwrap();
    let ResolvedShape::Rect { x, .. } = box_x.shape else {
        panic!("box should be a rect")
    };
    // 0.5 s along a track that covers 100 over 10 s.
    assert!(
        (x - 5.0).abs() < 1e-9,
        "at 11 s the box is at {x}, wanted 5"
    );
}

#[test]
fn a_chain_runs_at_its_own_rate_not_the_frame_rate() {
    // Links half a microsecond longer than a frame, so every end lands
    // just past a boundary and is noticed a touch early. Timed from the
    // instant each one should have started, the slack never piles up:
    // the chain keeps its own rate and slowly falls behind the frames,
    // which is what its durations say. Timed from the clock instead, it
    // would be rounded back onto a boundary every link and run fast.
    let show = r##"{
      "format": 1, "name": "cycle", "size": [8, 8],
      "layers": [{ "name": "box", "type": "shape",
                   "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF",
                   "timelines": [
        { "name": "a", "trigger": ["go", "d2"], "on_end": "d1", "hold": true,
          "tracks": [{ "property": "x",
                       "keys": [{"t":0,"v":0},{"t":0.0166671666666667,"v":1}] }] },
        { "name": "b", "trigger": "d1", "on_end": "d2", "hold": true,
          "tracks": [{ "property": "y",
                       "keys": [{"t":0,"v":0},{"t":0.0166671666666667,"v":1}] }] }
      ]}]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine.trigger("go");

    let mut ends = 0;
    for _ in 0..6000 {
        engine.advance_frame(1.0 / 60.0);
        ends += engine
            .drain_events()
            .iter()
            .filter(|e| matches!(e, Event::Trigger(name) if name.starts_with('d')))
            .count();
    }
    // 6000 frames is 100 s, which holds 5999.8 links of 0.0166671666666667.
    assert_eq!(ends, 5999);
}

#[test]
fn a_chain_hands_the_property_straight_over() {
    // Neither link holds, so between them nothing owns x and the layer
    // would fall back to its base. A handover that lands a rounding
    // error either side of a frame is still a handover, not a gap.
    let show = r##"{
      "format": 1, "name": "gap", "size": [8, 8],
      "layers": [{ "name": "box", "type": "shape",
                   "shape": { "rect": [0, 0, 1, 1] }, "fill": "#FFFFFF",
                   "x": -1,
                   "timelines": [
        { "name": "a", "trigger": ["go", "d2"], "on_end": "d1",
          "tracks": [{ "property": "x", "keys": [{"t":0,"v":10},{"t":0.7,"v":20}] }] },
        { "name": "b", "trigger": "d1", "on_end": "d2",
          "tracks": [{ "property": "x", "keys": [{"t":0,"v":20},{"t":0.7,"v":30}] }] }
      ]}]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine.trigger("go");

    for frame in 1..=4200 {
        engine.advance_frame(1.0 / 60.0);
        let layers = engine.resolved_layers().unwrap();
        let ResolvedShape::Rect { x, .. } = layers[0].shape else {
            panic!("box should be a rect")
        };
        assert!(
            x >= 10.0,
            "frame {frame}: nobody owns x, it fell back to {x}"
        );
    }
}

/// A panel with an animation started by a lamp rather than by name.
fn lit() -> Engine {
    let show = r##"{
      "name": "when", "size": [8, 8],
      "variables": { "lamp": 0, "mode": "" },
      "layers": [{
        "name": "panel", "type": "shape", "opacity": 0,
        "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF",
        "timelines": [
          { "name": "flash", "when": { "variable": "lamp", "threshold": 0.5 },
            "tracks": [{ "property": "opacity",
                         "keys": [{ "t": 0, "v": 1 }, { "t": 1, "v": 0 }] }] }
        ]
      }, {
        "name": "ball", "type": "shape", "opacity": 0,
        "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF",
        "timelines": [
          { "name": "multi", "when": { "variable": "mode", "map": { "multiball": 1 } },
            "tracks": [{ "property": "opacity",
                         "keys": [{ "t": 0, "v": 1 }, { "t": 1, "v": 1 }] }] }
        ]
      }]
    }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine
}

#[test]
fn a_timeline_starts_when_its_condition_becomes_true() {
    let mut engine = lit();
    engine.advance_frame(0.1);
    assert_eq!(engine.resolved_layers().unwrap()[0].opacity, 0.0);

    engine.set_variable("lamp", 1.0);
    engine.advance_frame(0.0);
    assert!((engine.resolved_layers().unwrap()[0].opacity - 1.0).abs() < 0.01);

    // Holding true does not start it again: it plays out and stays out.
    engine.advance_frame(0.5);
    let half = engine.resolved_layers().unwrap()[0].opacity;
    assert!((half - 0.5).abs() < 0.01, "{half}");
    engine.advance_frame(0.5);
    engine.advance_frame(0.5);
    assert_eq!(engine.resolved_layers().unwrap()[0].opacity, 0.0);

    // Off and on again is a new edge.
    engine.set_variable("lamp", 0.0);
    engine.advance_frame(0.016);
    engine.set_variable("lamp", 1.0);
    engine.advance_frame(0.0);
    assert!((engine.resolved_layers().unwrap()[0].opacity - 1.0).abs() < 0.01);
}

#[test]
fn a_condition_can_name_a_value_through_a_map() {
    let mut engine = lit();
    let ball = |e: &Engine| {
        e.resolved_layers()
            .unwrap()
            .iter()
            .find(|l| l.name == "ball")
            .unwrap()
            .opacity
    };
    engine.set_variable("mode", "skillshot");
    engine.advance_frame(0.016);
    assert_eq!(
        ball(&engine),
        0.0,
        "a value the map does not list is not true"
    );
    engine.set_variable("mode", "multiball");
    engine.advance_frame(0.016);
    assert_eq!(ball(&engine), 1.0);
}

#[test]
fn a_condition_needs_a_variable_and_a_finite_threshold() {
    let bad = r##"{
      "name": "when", "size": [8, 8],
      "layers": [{ "name": "p", "type": "shape", "shape": { "rect": [0, 0, 8, 8] },
                   "fill": "#FFFFFF",
                   "timelines": [{ "name": "t", "when": { "variable": "" },
                                   "tracks": [] }] }]
    }"##;
    assert!(Engine::new().load_show(bad).is_err());
}

/// A lamp with two animations: a flash on the rising edge, and a blink
/// that should run for as long as the lamp is lit.
const LIT: &str = r##"{
  "name": "lit", "size": [32, 32],
  "variables": { "lamp": 0 },
  "scenes": [
    { "name": "board", "trigger": "board", "layers": [{
      "name": "bulb", "type": "shape", "shape": { "rect": [0, 0, 8, 8] },
      "fill": "#FFFFFF", "x": 0, "y": 0,
      "timelines": [
        { "name": "flash", "when": { "variable": "lamp" }, "on_end": "flashed",
          "tracks": [{ "property": "x", "keys": [{"t":0,"v":0},{"t":0.2,"v":8}] }] },
        { "name": "blink", "loop": true, "while": { "variable": "lamp" },
          "tracks": [{ "property": "y", "keys": [{"t":0,"v":0},{"t":0.4,"v":8}] }] }
      ] }] },
    { "name": "away", "trigger": "away", "layers": [] }
  ] }"##;

#[test]
fn a_while_condition_runs_only_as_long_as_it_holds() {
    let mut engine = Engine::new();
    engine.load_show(LIT).unwrap();
    engine.trigger("board");
    engine.advance_to(0.1);
    // Dark: neither runs.
    let dark = engine.resolved_layers().unwrap();
    engine.set_variable("lamp", 1.0);
    engine.advance_to(0.2);
    let lit = engine.resolved_layers().unwrap();
    assert_ne!(dark, lit, "the lamp coming on should start something");

    // Lit and looping: y keeps moving across frames.
    let mut y = Vec::new();
    for i in 1..=6 {
        engine.advance_to(0.2 + f64::from(i) * 0.05);
        y.push(format!("{:?}", engine.resolved_layers().unwrap()[0].shape));
    }
    assert!(y.windows(2).any(|w| w[0] != w[1]), "the blink should run");

    // Off: the loop stops rather than blinking over a dark lamp.
    engine.set_variable("lamp", 0.0);
    engine.advance_to(0.6);
    let stopped = format!("{:?}", engine.resolved_layers().unwrap()[0].shape);
    for i in 1..=6 {
        engine.advance_to(0.6 + f64::from(i) * 0.05);
        let now = format!("{:?}", engine.resolved_layers().unwrap()[0].shape);
        assert_eq!(now, stopped, "the blink should have stopped");
    }
}

#[test]
fn an_edge_belongs_to_the_variable_not_to_the_scene() {
    let mut engine = Engine::new();
    engine.load_show(LIT).unwrap();
    engine.trigger("board");
    engine.set_variable("lamp", 1.0);
    engine.advance_to(0.05);
    let flashes = |engine: &mut Engine| {
        engine
            .drain_events()
            .iter()
            .filter(|e| matches!(e, Event::Trigger(n) if n == "flashed"))
            .count()
    };
    engine.advance_to(0.5);
    assert_eq!(flashes(&mut engine), 1, "the lamp came on once");

    // Away and back with the lamp still on: it did not come on again.
    engine.trigger("away");
    engine.advance_to(1.0);
    engine.trigger("board");
    engine.advance_to(1.5);
    assert_eq!(
        flashes(&mut engine),
        0,
        "coming back is not the lamp coming on"
    );

    // Away with the lamp still on, off and on again while away, and
    // back: it did come on, so the flash is owed. Turning it off before
    // leaving would pass whatever the engine remembered, which is how
    // this test first missed the case.
    engine.trigger("away");
    engine.advance_to(2.0);
    engine.set_variable("lamp", 0.0);
    engine.advance_to(2.1);
    engine.set_variable("lamp", 1.0);
    engine.advance_to(2.2);
    engine.trigger("board");
    engine.advance_to(2.6);
    assert_eq!(
        flashes(&mut engine),
        1,
        "it turned on while the scene was away"
    );
}
