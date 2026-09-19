//! Rendering through vello: needs a GPU adapter, tests skip (pass) when
//! none is available.

#![cfg(feature = "render")]

use cuelight::render::{RenderError, Renderer, RgbaFrame};
use cuelight::Engine;

fn render(engine: &Engine) -> Option<RgbaFrame> {
    match Renderer::new() {
        Ok(mut renderer) => Some(renderer.render_to_rgba(engine).unwrap()),
        Err(RenderError::NoAdapter) => {
            eprintln!("no GPU adapter, skipping");
            None
        }
        Err(e) => panic!("{e}"),
    }
}

fn pixel(frame: &RgbaFrame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * frame.width + x) * 4) as usize;
    frame.pixels[i..i + 4].try_into().unwrap()
}

#[test]
fn clipped_group_hides_children_outside_the_clip() {
    let show = r##"{ "name": "clip", "size": [16, 8], "background": "#000000", "layers": [
        { "name": "content", "type": "group", "x": 4, "clip": [8, 8], "children": [
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
