//! The presenter end to end on the GPU: fit, output mode and scaling.
//! Needs an adapter; skips (passes) without one, and on GitHub's Windows
//! runners, where vello renders crash (see tests/render.rs).

#![cfg(feature = "render")]

use cuelight::render::Presenter;
use cuelight::vello::{self, wgpu};
use cuelight::Engine;
use std::sync::{Mutex, MutexGuard, OnceLock};

mod gpu;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: vello::Renderer,
}

/// One device and renderer shared by every test, used under a lock: the
/// harness runs tests in parallel, and a vello renderer per test is enough
/// to exhaust a small machine's software adapter.
fn gpu() -> Option<MutexGuard<'static, Gpu>> {
    static GPU: OnceLock<Option<Mutex<Gpu>>> = OnceLock::new();
    let gpu = GPU.get_or_init(|| {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            gpu::no_adapter("the presenter tests");
            return None;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let renderer = vello::Renderer::new(&device, vello::RendererOptions::default()).unwrap();
        Some(Mutex::new(Gpu {
            device,
            queue,
            renderer,
        }))
    });
    Some(gpu.as_ref()?.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Present `engine` into a `width` x `height` target and read it back.
fn present(
    gpu: &mut Gpu,
    presenter: &mut Presenter,
    engine: &Engine,
    [width, height]: [u32; 2],
) -> Vec<[u8; 4]> {
    let presented = presenter
        .present(
            engine,
            &gpu.device,
            &gpu.queue,
            &mut gpu.renderer,
            [width, height],
        )
        .unwrap();
    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    gpu.renderer
        .render_to_texture(
            &gpu.device,
            &gpu.queue,
            &presented.scene,
            &target.create_view(&Default::default()),
            &vello::RenderParams {
                base_color: presented.base_color,
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .unwrap();
    let bytes_per_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(bytes_per_row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: None,
            },
        },
        extent,
    );
    gpu.queue.submit([encoder.finish()]);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let mapped = buffer.slice(..).get_mapped_range();
    let mut pixels = Vec::new();
    for row in 0..height {
        let start = (row * bytes_per_row) as usize;
        let line = &mapped[start..start + (width * 4) as usize];
        pixels.extend(line.as_chunks::<4>().0.iter().copied());
    }
    pixels
}

// 4x2 canvas: left half white, right half mid gray, on black.
const SHOW: &str = r##"{ "name": "p", "size": [4, 2], "background": "#000000",
  "output": { "mode": "gray4", "tint": "#FF8000", "scaling": "pixel_perfect" },
  "layers": [
    { "name": "white", "type": "shape", "shape": { "rect": [0, 0, 2, 2] }, "fill": "#FFFFFF" },
    { "name": "gray", "type": "shape", "shape": { "rect": [2, 0, 2, 2] }, "fill": "#808080" }
  ],
  "scenes": [ { "name": "dmd", "trigger": "dmd" },
              { "name": "color", "trigger": "color", "output": { "mode": "rgb", "scaling": "smooth" } } ] }"##;

#[test]
fn gray_pixel_perfect_show_lands_as_tinted_blocks() {
    let Some(mut gpu) = gpu() else { return };
    let mut engine = Engine::new();
    engine.load_show(SHOW).unwrap();
    let mut presenter = Presenter::new();
    // 18x12 target: 4x scale fits (16x8), centered at (1, 2), letterboxed.
    let (w, h) = (18u32, 12u32);
    let pixels = present(&mut gpu, &mut presenter, &engine, [w, h]);
    let at = |x: u32, y: u32| pixels[(y * w + x) as usize];
    // #808080 is gray level 8 of 15: 255 * 8 / 15 = 136, 128 * 8 / 15 = 68
    let (white, gray, black) = ([255, 128, 0, 255], [136, 68, 0, 255], [0, 0, 0, 255]);
    for (x, y, expected) in [
        (1, 2, white),
        (8, 9, white),
        (9, 2, gray),
        (16, 9, gray), // block corners
        (0, 5, black),
        (17, 5, black),
        (8, 1, black),
        (8, 10, black), // letterbox
    ] {
        assert_eq!(at(x, y), expected, "pixel ({x}, {y})");
    }
    // Crisp: the pixel left of the white/gray boundary is pure white.
    assert_eq!((at(8, 5), at(9, 5)), (white, gray));
}

#[test]
fn a_scene_can_switch_to_full_color_and_back() {
    let Some(mut gpu) = gpu() else { return };
    let mut engine = Engine::new();
    engine.load_show(SHOW).unwrap();
    let mut presenter = Presenter::new();
    let center_right = |pixels: &[[u8; 4]]| pixels[(4 * 16 + 12) as usize];
    let first = present(&mut gpu, &mut presenter, &engine, [16, 8]);
    assert_eq!(center_right(&first), [136, 68, 0, 255]);
    engine.trigger("color");
    let color = present(&mut gpu, &mut presenter, &engine, [16, 8]);
    assert_eq!(center_right(&color), [128, 128, 128, 255]);
    engine.trigger("dmd");
    let back = present(&mut gpu, &mut presenter, &engine, [16, 8]);
    assert_eq!(center_right(&back), [136, 68, 0, 255]);
}

#[test]
fn pixel_perfect_scaling_never_blends_canvas_pixels() {
    // Anti-aliased content (a circle, segment digits) on a 64x16 canvas,
    // shown in a target that is no multiple of it: every canvas pixel has
    // to come out as one uniform block, none stretched or blended.
    const DMD: &str = r##"{ "name": "d", "size": [64, 16], "background": "#000000",
      "output": { "mode": "gray4", "tint": "#FF8000", "scaling": "pixel_perfect" },
      "layers": [
        { "name": "dot", "type": "shape", "shape": { "circle": [52.3, 8.4, 5.5] }, "fill": "#FFFFFF" },
        { "name": "score", "type": "digits", "digits": 3, "size": [36, 14], "x": 2, "y": 1,
          "text": "128",
          "display": { "segments": { "style": "numeric7", "fill": "#FFFFFF", "unlit": "#303030" } } }
      ] }"##;
    let Some(mut gpu) = gpu() else { return };
    let mut engine = Engine::new();
    engine.load_show(DMD).unwrap();
    let mut presenter = Presenter::new();
    // 3x fits (192x48), letterboxed at (11, 4) in 215x57.
    let (w, h, k, left, top) = (215u32, 57u32, 3u32, 11u32, 4u32);
    let pixels = present(&mut gpu, &mut presenter, &engine, [w, h]);
    let at = |x: u32, y: u32| pixels[(y * w + x) as usize];
    let mut lit = 0;
    for by in 0..16 {
        for bx in 0..64 {
            let (x0, y0) = (left + bx * k, top + by * k);
            let block = at(x0, y0);
            lit += u32::from(block != [0, 0, 0, 255]);
            for (dx, dy) in [(1, 0), (2, 0), (0, 1), (0, 2), (2, 2)] {
                assert_eq!(at(x0 + dx, y0 + dy), block, "canvas pixel ({bx}, {by})");
            }
        }
    }
    assert!(lit > 50, "the content should be there: {lit} lit pixels");
    // And nothing outside the 192x48 picture.
    assert_eq!(at(left - 1, 20), [0, 0, 0, 255]);
    assert_eq!(at(left + 192, 20), [0, 0, 0, 255]);
}

#[test]
fn dots_pass_shows_canvas_pixels_as_dots_on_black() {
    // 8x4 canvas, left half lit, shown at 10 surface pixels per dot.
    const DOTS: &str = r##"{ "name": "d", "size": [8, 4], "background": "#000000",
      "output": { "mode": "gray4", "tint": "#FF8000",
                  "passes": [ { "dots": { "size": 0.8, "unlit": "#301000" } } ] },
      "layers": [
        { "name": "lit", "type": "shape", "shape": { "rect": [0, 0, 4, 4] }, "fill": "#FFFFFF" }
      ],
      "scenes": [ { "name": "on", "trigger": "on" },
                  { "name": "plain", "trigger": "plain", "output": { "passes": [] } } ] }"##;
    let Some(mut gpu) = gpu() else { return };
    let mut engine = Engine::new();
    engine.load_show(DOTS).unwrap();
    let mut presenter = Presenter::new();
    let (w, h) = (80u32, 40u32);
    let pixels = present(&mut gpu, &mut presenter, &engine, [w, h]);
    if let Some(path) = std::env::var_os("CUELIGHT_DUMP_DOTS") {
        let frame = cuelight::render::RgbaFrame {
            width: w,
            height: h,
            pixels: pixels.iter().flatten().copied().collect(),
        };
        frame.write_png(path).unwrap();
    }
    let at = |x: u32, y: u32| pixels[(y * w + x) as usize];
    let near = |a: [u8; 4], b: [u8; 4]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 2);
    // The middle of a lit dot, the dark between four dots, the middle of a
    // dot that is off.
    assert!(near(at(5, 5), [255, 128, 0, 255]), "{:?}", at(5, 5));
    assert!(near(at(10, 10), [0, 0, 0, 255]), "{:?}", at(10, 10));
    assert!(near(at(75, 5), [0x30, 0x10, 0, 255]), "{:?}", at(75, 5));

    // A scene can turn the passes off again.
    engine.trigger("plain");
    let plain = present(&mut gpu, &mut presenter, &engine, [w, h]);
    assert!(near(plain[(10 * w + 10) as usize], [255, 128, 0, 255]));
}

#[test]
fn nothing_outside_the_canvas_reaches_the_letterbox() {
    let Some(mut gpu) = gpu() else { return };
    // A 4x2 full-color show with a white shape parked below its canvas.
    let show = r##"{ "name": "parked", "size": [4, 2], "background": "#000000", "layers": [
        { "name": "sun", "type": "shape", "shape": { "rect": [0, 2, 4, 4] }, "fill": "#FFFFFF" },
        { "name": "in", "type": "shape", "shape": { "rect": [0, 0, 4, 2] }, "fill": "#FF0000" }
    ] }"##;
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    let mut presenter = Presenter::new();
    // 8x8 target: 2x scale, the canvas on rows 2 to 5, letterbox around.
    let (w, h) = (8u32, 8u32);
    let pixels = present(&mut gpu, &mut presenter, &engine, [w, h]);
    let at = |x: u32, y: u32| pixels[(y * w + x) as usize];
    assert_eq!(at(4, 2), [255, 0, 0, 255]);
    assert_eq!(at(4, 5), [255, 0, 0, 255]);
    // Below the canvas the parked shape would have painted; it must not.
    assert_eq!(at(4, 6), [0, 0, 0, 255]);
    assert_eq!(at(4, 7), [0, 0, 0, 255]);
}
