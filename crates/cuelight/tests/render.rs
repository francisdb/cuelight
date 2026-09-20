//! Rendering through vello: needs a GPU adapter, tests skip (pass) when
//! none is available.

#![cfg(feature = "render")]

use cuelight::render::{RenderError, Renderer, RgbaFrame};
use cuelight::Engine;
use std::sync::{Mutex, OnceLock};

/// One renderer shared by every test, used under a lock: the harness runs
/// tests on parallel threads, and creating GPU devices concurrently (or
/// tearing them down at thread exit) crashes some software adapters.
fn render(engine: &Engine) -> Option<RgbaFrame> {
    // On GitHub's Windows runners these renders die with an access
    // violation (cause unknown; a Windows 10 VM with the same DX12 software
    // adapter runs them fine). Linux and macOS CI, and any other Windows
    // machine, still run them.
    if cfg!(windows) && std::env::var_os("CI").is_some() {
        eprintln!("software adapter on Windows CI, skipping");
        return None;
    }
    static RENDERER: OnceLock<Option<Mutex<Renderer>>> = OnceLock::new();
    let renderer = RENDERER.get_or_init(|| match Renderer::new() {
        Ok(renderer) => Some(Mutex::new(renderer)),
        Err(RenderError::NoAdapter) => {
            eprintln!("no GPU adapter, skipping");
            None
        }
        Err(e) => panic!("{e}"),
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
