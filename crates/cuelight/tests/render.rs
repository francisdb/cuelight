//! Rendering through vello. These need a GPU adapter and fail without
//! one; `CUELIGHT_SKIP_GPU_TESTS=1` skips them on purpose.

#![cfg(feature = "render")]

use cuelight::render::{RenderError, Renderer, RgbaFrame};
use cuelight::Engine;
use std::sync::{Mutex, OnceLock};

mod gpu;

/// One renderer shared by every test, used under a lock: the harness runs
/// tests on parallel threads, and creating GPU devices concurrently (or
/// tearing them down at thread exit) crashes some software adapters.
fn render(engine: &Engine) -> Option<RgbaFrame> {
    static RENDERER: OnceLock<Option<Mutex<Renderer>>> = OnceLock::new();
    let renderer = RENDERER.get_or_init(|| {
        // Nothing initialises a logger in a test binary, so wgpu's account
        // of which adapter and backend it picked goes nowhere. Without
        // RUST_LOG set this prints nothing and costs nothing; with it, a
        // run that dies says what it was talking to (see issue #23).
        let _ = env_logger::builder().is_test(false).try_init();
        match Renderer::new() {
            Ok(renderer) => {
                // Named before anything is drawn, so a run that dies
                // during the first render still says what it was talking
                // to (see issue #23).
                eprintln!("render adapter: {:?}", renderer.adapter());
                Some(Mutex::new(renderer))
            }
            Err(RenderError::NoAdapter) => {
                gpu::no_adapter("the render tests");
                None
            }
            Err(e) => panic!("{e}"),
        }
    });
    let mut renderer = renderer.as_ref()?.lock().unwrap_or_else(|e| e.into_inner());
    Some(renderer.render_to_rgba(engine).unwrap())
}

fn pixel(frame: &RgbaFrame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * frame.width + x) * 4) as usize;
    frame.pixels[i..i + 4].try_into().unwrap()
}

#[test]
fn clipped_group_hides_children_outside_the_clip() {
    let show = r##"{ "name": "clip", "size": [16, 8], "background": "#000000", "layers": [
        { "name": "content", "type": "group", "x": 4, "clip": { "rect": [0, 0, 8, 8] }, "children": [
            { "name": "wide", "type": "shape", "shape": { "rect": [-4, 0, 16, 8] }, "fill": "#FFFFFF" }
        ] }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    assert_eq!(pixel(&frame, 1, 4), [0, 0, 0, 255]);
    assert_eq!(pixel(&frame, 6, 4), [255, 255, 255, 255]);
    assert_eq!(pixel(&frame, 14, 4), [0, 0, 0, 255]);
}

#[test]
fn circular_clip_cuts_the_corners() {
    let show = r##"{ "name": "clip", "size": [16, 16], "background": "#000000", "layers": [
        { "name": "porthole", "type": "group", "clip": { "circle": [8, 8, 6] }, "children": [
            { "name": "fill", "type": "shape", "shape": { "rect": [0, 0, 16, 16] }, "fill": "#FFFFFF" }
        ] }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    assert_eq!(pixel(&frame, 8, 8), [255, 255, 255, 255]);
    assert_eq!(pixel(&frame, 1, 1), [0, 0, 0, 255]);
}

#[test]
fn sheet_cell_lands_on_the_destination() {
    // 4x2 image: left 2x2 cell red, right 2x2 cell green.
    let mut pixels = Vec::new();
    for _y in 0..2 {
        for x in 0..4 {
            pixels.extend_from_slice(if x < 2 {
                &[255, 0, 0, 255]
            } else {
                &[0, 255, 0, 255]
            });
        }
    }
    let show = r##"{ "name": "s", "size": [8, 8], "layers": [
        { "name": "cell", "type": "image", "image": "cells", "x": 2, "y": 2,
          "sheet": { "cell": [2, 2], "columns": 2 }, "frame": 1, "size": [4, 4] }
    ] }"##;
    let mut engine = Engine::new();
    engine.set_image("cells", 4, 2, pixels).unwrap();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    // The whole 4x4 destination is the green cell, nothing outside it.
    for (x, y) in [(2, 2), (5, 2), (2, 5), (5, 5), (3, 4)] {
        assert_eq!(pixel(&frame, x, y), [0, 255, 0, 255], "pixel ({x}, {y})");
    }
    assert_eq!(pixel(&frame, 1, 3), [0, 0, 0, 255]);
    assert_eq!(pixel(&frame, 6, 3), [0, 0, 0, 255]);
}

#[test]
fn images_survive_frames_without_images() {
    let show = r##"{ "name": "s", "size": [8, 8], "background": "#000000", "scenes": [
        { "name": "picture", "trigger": "picture", "layers": [
            { "name": "img", "type": "image", "image": "white", "size": [8, 8] } ] },
        { "name": "plain", "trigger": "plain", "layers": [
            { "name": "box", "type": "shape", "shape": { "rect": [0, 0, 2, 2] }, "fill": "#FF0000" } ] }
    ] }"##;
    let mut engine = Engine::new();
    engine.set_image("white", 1, 1, vec![255u8; 4]).unwrap();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    assert_eq!(pixel(&frame, 6, 6), [255, 255, 255, 255]);
    // A frame that draws no image at all...
    engine.trigger("plain");
    let frame = render(&engine).unwrap();
    assert_eq!(pixel(&frame, 6, 6), [0, 0, 0, 255]);
    // ...must not lose the images for the frames after it.
    engine.trigger("picture");
    let frame = render(&engine).unwrap();
    assert_eq!(pixel(&frame, 6, 6), [255, 255, 255, 255]);
}

#[cfg(feature = "outline-fonts")]
#[test]
fn outline_text_fills_its_glyphs_and_borders_them() {
    let show = r##"{ "name": "t", "size": [96, 96], "background": "#000000",
      "fonts": { "big": { "file": "sans", "size": 120, "color": "#FFFFFF",
                          "border": { "color": "#FF0000", "width": 3 } } },
      "layers": [ { "name": "l", "type": "text", "font": "big", "text": "l", "size": [96, 96] } ] }"##;
    let mut engine = Engine::new();
    engine
        .set_outline_font(
            "sans",
            include_bytes!("fonts/cuelight_test_sans.ttf").as_slice(),
        )
        .unwrap();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    // A lowercase l is a vertical bar through the middle of the canvas:
    // white inside, a red border beside it, background further out.
    assert_eq!(pixel(&frame, 48, 48), [255, 255, 255, 255]);
    let row: Vec<[u8; 4]> = (0..96).map(|x| pixel(&frame, x, 48)).collect();
    assert!(row.contains(&[255, 0, 0, 255]), "no border: {row:?}");
    assert_eq!(row[4], [0, 0, 0, 255]);
    assert_eq!(row[91], [0, 0, 0, 255]);
}

#[test]
fn a_path_fills_inside_its_outline() {
    // A right triangle covering the lower-left half of the canvas.
    let show = r##"{ "name": "path", "size": [16, 16], "background": "#000000", "layers": [
        { "name": "tri", "type": "shape", "shape": { "path": "M0 0 L0 16 L16 16 Z" }, "fill": "#FFFFFF" }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    assert_eq!(pixel(&frame, 3, 12), [255, 255, 255, 255]);
    assert_eq!(pixel(&frame, 12, 3), [0, 0, 0, 255]);
}

#[test]
fn a_stroke_outlines_a_shape() {
    // A transparent rect with a 2px stroke: the edge paints, the inside
    // and the outside do not.
    let show = r##"{ "name": "stroke", "size": [16, 16], "background": "#000000", "layers": [
        { "name": "box", "type": "shape", "shape": { "rect": [4, 4, 8, 8] }, "fill": "#00000000",
          "stroke": { "color": "#00FF00", "width": 2 } }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    assert_eq!(pixel(&frame, 4, 8), [0, 255, 0, 255]);
    assert_eq!(pixel(&frame, 8, 8), [0, 0, 0, 255]);
    assert_eq!(pixel(&frame, 1, 8), [0, 0, 0, 255]);
}

#[test]
fn rotated_shapes_render_where_the_transform_says() {
    // A 12x2 white bar anchored at its center, turned upright.
    let show = r##"{ "name": "rotate", "size": [16, 16], "background": "#000000", "layers": [
        { "name": "bar", "type": "shape", "shape": { "rect": [0, 0, 12, 2] }, "fill": "#FFFFFF",
          "x": 8, "y": 8, "anchor": "center", "rotation": 90 }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    assert_eq!(pixel(&frame, 7, 3), [255, 255, 255, 255]);
    assert_eq!(pixel(&frame, 7, 12), [255, 255, 255, 255]);
    assert_eq!(pixel(&frame, 3, 7), [0, 0, 0, 255]);
    assert_eq!(pixel(&frame, 12, 7), [0, 0, 0, 255]);
}

#[test]
fn a_rotated_group_turns_its_clip_and_children() {
    let show = r##"{ "name": "rotate", "size": [16, 16], "background": "#000000", "layers": [
        { "name": "window", "type": "group", "x": 8, "y": 8, "rotation": 90, "clip": { "rect": [0, 0, 6, 2] },
          "children": [ { "name": "fill", "type": "shape", "shape": { "rect": [-8, -8, 16, 16] }, "fill": "#FFFFFF" } ] }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    // The clip's 6x2 window, turned clockwise, runs down from (8, 8) and
    // extends to the left.
    assert_eq!(pixel(&frame, 7, 11), [255, 255, 255, 255]);
    assert_eq!(pixel(&frame, 9, 11), [0, 0, 0, 255]);
    assert_eq!(pixel(&frame, 7, 5), [0, 0, 0, 255]);
}

#[test]
fn blend_modes_combine_with_what_is_beneath() {
    // A mid gray base with three gray squares over it, one per blend.
    let show = r##"{ "name": "blend", "size": [24, 8], "background": "#808080", "layers": [
        { "name": "add", "type": "shape", "shape": { "rect": [0, 0, 8, 8] }, "fill": "#404040", "blend": "add" },
        { "name": "screen", "type": "shape", "shape": { "rect": [8, 0, 8, 8] }, "fill": "#808080", "blend": "screen" },
        { "name": "multiply", "type": "shape", "shape": { "rect": [16, 0, 8, 8] }, "fill": "#808080", "blend": "multiply" }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    let near = |[r, g, b, a]: [u8; 4], want: u8| {
        assert!(
            a == 255 && [r, g, b].iter().all(|c| c.abs_diff(want) <= 2),
            "{:?} vs {want}",
            [r, g, b]
        );
    };
    // 0.5 + 0.25
    near(pixel(&frame, 4, 4), 192);
    // 1 - 0.5 * 0.5
    near(pixel(&frame, 12, 4), 191);
    // 0.5 * 0.5
    near(pixel(&frame, 20, 4), 64);
}

#[test]
fn a_blended_group_is_composited_as_one_picture() {
    // Two overlapping white shapes in an additive group: inside the group
    // they paint over each other (one white), and the group adds once.
    let show = r##"{ "name": "group", "size": [8, 8], "background": "#404040", "layers": [
        { "name": "glow", "type": "group", "blend": "add", "children": [
            { "name": "a", "type": "shape", "shape": { "rect": [0, 0, 8, 8] }, "fill": "#404040" },
            { "name": "b", "type": "shape", "shape": { "rect": [0, 0, 8, 8] }, "fill": "#404040" }
        ] }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    let [r, g, b, _] = pixel(&frame, 4, 4);
    // 0.25 + 0.25, not 0.25 + 0.25 + 0.25.
    assert!(
        [r, g, b].iter().all(|c| c.abs_diff(128) <= 2),
        "{:?}",
        [r, g, b]
    );
}

#[test]
fn a_tint_stains_an_image_and_leaves_its_transparency() {
    // A 2x1 image: an opaque white pixel and a transparent one.
    let show = r##"{ "name": "tint", "size": [4, 2], "background": "#0000FF", "layers": [
        { "name": "art", "type": "image", "image": "art", "size": [4, 2], "tint": "#FF8000" }
    ] }"##;
    let mut engine = Engine::new();
    engine
        .set_image("art", 2, 1, vec![255, 255, 255, 255, 255, 255, 255, 0])
        .unwrap();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    // White times the tint is the tint; the clear half keeps the background.
    assert_eq!(pixel(&frame, 0, 0), [255, 128, 0, 255]);
    assert_eq!(pixel(&frame, 3, 0), [0, 0, 255, 255]);
}

#[test]
fn an_untinted_image_is_unchanged() {
    let show = r##"{ "name": "plain", "size": [4, 2], "background": "#0000FF", "layers": [
        { "name": "art", "type": "image", "image": "art", "size": [4, 2] }
    ] }"##;
    let mut engine = Engine::new();
    engine
        .set_image("art", 1, 1, vec![255, 128, 0, 255])
        .unwrap();
    engine.load_show(show).unwrap();
    let Some(frame) = render(&engine) else { return };
    assert_eq!(pixel(&frame, 0, 0), [255, 128, 0, 255]);
    assert_eq!(pixel(&frame, 3, 1), [255, 128, 0, 255]);
}
