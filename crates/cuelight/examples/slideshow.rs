//! Slideshow demo: three procedurally generated images (a rainbow, TV
//! noise and a sine wave), crossfaded every two seconds, looping forever.
//!
//! The images are generated in memory and handed to the engine with
//! `Engine::set_image`; the show's looping autoplay timelines do the rest,
//! no external events required. Escape quits.
//!
//! The images are generated oversampled at the window's effective
//! resolution (device pixels per show unit, fetched from the window at
//! startup) and mapped back down through the image layers' declared
//! `size`, so they stay crisp on hidpi displays where the window surface
//! outresolves the 480x270 canvas.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cuelight::render::{background_color, build_vello_scene, ImageCache};
use cuelight::vello;
use cuelight::Engine;
use vello::kurbo::Affine;
use vello::util::{RenderContext, RenderSurface};
use vello::wgpu;
use wgpu::CurrentSurfaceTexture;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

mod common;

/// Horizontal hue sweep, full saturation, dimming toward the bottom.
fn rainbow(w: u32, h: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let value = 1.0 - 0.6 * f64::from(y) / f64::from(h - 1);
        for x in 0..w {
            let hue = 360.0 * f64::from(x) / f64::from(w - 1);
            let [r, g, b] = hsv_to_rgb(hue, 1.0, value);
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    px
}

/// Grayscale static, xorshift-generated (no rand dependency).
fn tv_noise(w: u32, h: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    let mut state: u32 = 0x2545_F491;
    for _ in 0..w * h {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let v = (state >> 24) as u8;
        px.extend_from_slice(&[v, v, v, 255]);
    }
    px
}

/// A bright sine trace on a dark blue background.
fn sine_wave(w: u32, h: u32, oversample: f64) -> Vec<u8> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    let amplitude = f64::from(h) * 0.35;
    let mid = f64::from(h) / 2.0;
    for y in 0..h {
        for x in 0..w {
            let phase = f64::from(x) / f64::from(w) * std::f64::consts::TAU * 2.0;
            let curve_y = mid - phase.sin() * amplitude;
            // Trace thickness in image pixels: a 2-show-unit core fading
            // out over 2 more, regardless of oversampling.
            let dist = (f64::from(y) - curve_y).abs() / oversample;
            let glow = (1.0 - (dist - 2.0) / 2.0).clamp(0.0, 1.0);
            let r = (16.0 + 64.0 * glow) as u8;
            let g = (24.0 + 231.0 * glow) as u8;
            let b = (48.0 + 150.0 * glow) as u8;
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    px
}

/// `h` in degrees, `s`/`v` in [0, 1].
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [u8; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 % 360 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    ]
}

struct RenderState {
    window: Arc<Window>,
    surface: RenderSurface<'static>,
}

struct App {
    engine: Engine,
    context: RenderContext,
    // One vello renderer per wgpu device the context hands out.
    renderers: Vec<Option<vello::Renderer>>,
    state: Option<RenderState>,
    /// The window's newest size, waiting for the next redraw.
    pending_size: Option<(u32, u32)>,
    occluded: bool,
    last_frame: Instant,
    fps: common::Fps,
    images: ImageCache,
}

impl App {
    fn redraw(&mut self) {
        let Some(state) = &mut self.state else { return };

        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64().min(0.1);
        self.last_frame = now;
        self.engine.advance_frame(dt);

        // Fit the show into the window: uniform scale, centered.
        let [show_w, show_h] = self.engine.show().expect("show loaded").size;
        if let Some((width, height)) = self.pending_size.take() {
            let config = &state.surface.config;
            if (width, height) != (config.width, config.height) {
                self.context
                    .resize_surface(&mut state.surface, width, height);
            }
        }
        let surface = &state.surface;
        let (sw, sh) = (surface.config.width, surface.config.height);
        let scale = (f64::from(sw) / f64::from(show_w)).min(f64::from(sh) / f64::from(show_h));
        let tx = (f64::from(sw) - f64::from(show_w) * scale) / 2.0;
        let ty = (f64::from(sh) - f64::from(show_h) * scale) / 2.0;
        let mut frame = vello::Scene::new();
        frame.append(
            &build_vello_scene(&self.engine, &mut self.images).expect("build vello scene"),
            Some(Affine::translate((tx, ty)) * Affine::scale(scale)),
        );
        self.fps.tick();
        self.fps.draw(&mut frame, state.window.scale_factor());

        let device_handle = &self.context.devices[surface.dev_id];
        self.renderers[surface.dev_id]
            .as_mut()
            .expect("renderer for surface device")
            .render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                &frame,
                &surface.target_view,
                &vello::RenderParams {
                    base_color: background_color(&self.engine),
                    width: sw,
                    height: sh,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .expect("vello render");

        // Blit the intermediate target to the window surface and present.
        let surface_texture = match surface.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(t) | CurrentSurfaceTexture::Suboptimal(t) => t,
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                // Reconfigure and skip this frame; the next redraw retries.
                self.context.configure_surface(surface);
                state.window.request_redraw();
                return;
            }
            CurrentSurfaceTexture::Timeout
            | CurrentSurfaceTexture::Occluded
            | CurrentSurfaceTexture::Validation => {
                // Retry throttled: without vsync pacing this path would
                // otherwise spin at full speed.
                std::thread::sleep(Duration::from_millis(100));
                state.window.request_redraw();
                return;
            }
        };
        let mut encoder =
            device_handle
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("cuelight-surface-blit"),
                });
        surface.blitter.copy(
            &device_handle.device,
            &mut encoder,
            &surface.target_view,
            &surface_texture
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        );
        device_handle.queue.submit([encoder.finish()]);
        surface_texture.present();

        state.window.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let [w, h] = self.engine.show().expect("show loaded").size;
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("cuelight: slideshow")
                        .with_inner_size(LogicalSize::new(w * 2, h * 2)),
                )
                .expect("create window"),
        );
        common::log_window_info(&window);
        let size = window.inner_size();
        let surface = pollster::block_on(self.context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::AutoVsync,
        ))
        .expect("create surface");
        common::log_adapter(&self.context, surface.dev_id);
        self.renderers
            .resize_with(self.context.devices.len(), || None);
        self.renderers[surface.dev_id].get_or_insert_with(|| {
            vello::Renderer::new(
                &self.context.devices[surface.dev_id].device,
                vello::RendererOptions::default(),
            )
            .expect("create vello renderer")
        });

        // Generate the slides now that the window exists: oversample by the
        // window's device pixels per show unit (hidpi factor included) so
        // the images resolve 1:1 on the surface. The show's image layers
        // were skipped until now; they pick the pixels up next frame.
        let oversample = (f64::from(size.width.max(1)) / f64::from(w))
            .min(f64::from(size.height.max(1)) / f64::from(h))
            .ceil()
            .max(1.0);
        let (iw, ih) = (w * oversample as u32, h * oversample as u32);
        self.engine
            .set_image("rainbow", iw, ih, rainbow(iw, ih))
            .expect("register rainbow");
        self.engine
            .set_image("noise", iw, ih, tv_noise(iw, ih))
            .expect("register noise");
        self.engine
            .set_image("sine", iw, ih, sine_wave(iw, ih, oversample))
            .expect("register sine");
        log::info!("generated 3 slide images at {iw}x{ih} ({oversample}x oversample)");

        self.state = Some(RenderState { window, surface });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                log::info!("close requested, exiting");
                event_loop.exit();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if let Key::Named(NamedKey::Escape) = event.logical_key {
                    log::info!("escape pressed, exiting");
                    event_loop.exit();
                }
            }
            // Only noted here: a drag can deliver several sizes per frame
            // and reconfiguring the surface costs milliseconds each time.
            // The next redraw applies the last one.
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    self.pending_size = Some((size.width, size.height));
                }
            }
            WindowEvent::Occluded(occluded) => {
                log::info!("window occluded: {occluded}");
                self.occluded = occluded;
                if !occluded {
                    if let Some(state) = &self.state {
                        state.window.request_redraw();
                    }
                }
            }
            // While occluded the redraw chain stops entirely; the
            // Occluded(false) event restarts it.
            WindowEvent::RedrawRequested if !self.occluded => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if common::ctrl_c_pressed() {
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(if self.occluded {
            // No redraws while hidden: wake periodically so ctrl-c still
            // exits promptly.
            ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(200))
        } else {
            ControlFlow::Wait
        });
    }
}

fn main() -> std::process::ExitCode {
    common::init_logging();
    common::install_ctrl_c_handler();
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    log::info!("starting slideshow example");
    let json = include_str!("shows/slideshow.json");

    let mut engine = Engine::new();
    engine.load_show(json)?;

    let mut app = App {
        engine,
        context: RenderContext::new(),
        renderers: Vec::new(),
        state: None,
        pending_size: None,
        occluded: false,
        last_frame: Instant::now(),
        fps: common::Fps::new(),
        images: ImageCache::new(),
    };
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;
    log::info!("event loop finished, exiting");
    Ok(())
}
