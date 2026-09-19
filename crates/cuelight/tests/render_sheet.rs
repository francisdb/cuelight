//! Sprite sheet cells through vello: needs a GPU adapter, skips (passes)
//! when none is available.

#![cfg(feature = "render")]

use cuelight::render::{RenderError, Renderer};
use cuelight::Engine;

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
    let mut renderer = match Renderer::new() {
        Ok(r) => r,
        Err(RenderError::NoAdapter) => return eprintln!("no GPU adapter, skipping"),
        Err(e) => panic!("{e}"),
    };
    let frame = renderer.render_to_rgba(&engine).unwrap();
    let px = |x: u32, y: u32| {
        let i = ((y * frame.width + x) * 4) as usize;
        [frame.pixels[i], frame.pixels[i + 1], frame.pixels[i + 2]]
    };
    // Whole 4x4 destination is the green cell, nothing outside it.
    for (x, y) in [(2, 2), (5, 2), (2, 5), (5, 5), (3, 4)] {
        assert_eq!(px(x, y), [0, 255, 0], "pixel ({x}, {y})");
    }
    assert_eq!(px(1, 3), [0, 0, 0]);
    assert_eq!(px(6, 3), [0, 0, 0]);
}
