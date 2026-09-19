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
    // The Windows CI runners only offer a software adapter (WARP), and
    // vello's compute pipeline crashes it with an access violation. Linux
    // and macOS CI, and Windows machines with a real GPU, still run these.
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
