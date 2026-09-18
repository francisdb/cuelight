//! Windowed demo: the same minigolf show as `render_to_images`, rendered
//! into a winit window instead of PNG dumps.
//!
//! The example drives the engine continuously: the `score` variable pulses
//! with a slow sine (the bound bar fades with it) and the `go` trigger
//! re-fires every two seconds so the ball keeps rolling. Press space to
//! fire the trigger yourself, escape to quit.
//!
//! Rendering goes through vello's surface helpers: the show is rendered
//! into an intermediate texture and blitted to the window surface, scaled
//! uniformly to fit the window.

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

/// Seconds between automatic re-fires of the `go` trigger.
const GO_INTERVAL: f64 = 2.0;

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
    occluded: bool,
    started: Instant,
    last_frame: Instant,
    next_go: f64,
    fps: common::Fps,
    images: ImageCache,
}

impl App {
    fn redraw(&mut self) {
        let Some(state) = &self.state else { return };

        // Drive the engine: measured dt, a pulsing score and a periodic
        // trigger so the window keeps animating.
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64().min(0.1);
        self.last_frame = now;
        let elapsed = now.duration_since(self.started).as_secs_f64();
        self.engine
            .set_variable("score", 500.0 + 500.0 * (elapsed * 0.8).sin());
        if elapsed >= self.next_go {
            log::debug!("auto-firing trigger \"go\"");
            self.engine.trigger("go");
            self.next_go = elapsed + GO_INTERVAL;
        }
        self.engine.advance_frame(dt);

        // Fit the show into the window: uniform scale, centered.
        let [show_w, show_h] = self.engine.show().expect("show loaded").size;
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
                        .with_title("cuelight: minigolf")
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
        self.state = Some(RenderState { window, surface });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                log::info!("close requested, exiting");
                event_loop.exit();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        log::info!("escape pressed, exiting");
                        event_loop.exit();
                    }
                    Key::Named(NamedKey::Space) => {
                        log::info!("space pressed, firing trigger \"go\"");
                        self.engine.trigger("go");
                    }
                    _ => {}
                }
            }
            WindowEvent::Resized(size) => {
                log::debug!("resized to {}x{} physical", size.width, size.height);
                if let Some(state) = &mut self.state {
                    if size.width > 0 && size.height > 0 {
                        self.context
                            .resize_surface(&mut state.surface, size.width, size.height);
                    }
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
    log::info!("starting render_to_window example");
    let json = include_str!("shows/minigolf.json");

    let mut engine = Engine::new();
    engine.load_show(json)?;
    engine.set_variable("score", 500.0);
    engine.trigger("go");

    let now = Instant::now();
    let mut app = App {
        engine,
        context: RenderContext::new(),
        renderers: Vec::new(),
        state: None,
        occluded: false,
        started: now,
        last_frame: now,
        next_go: GO_INTERVAL,
        fps: common::Fps::new(),
        images: ImageCache::new(),
    };
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;
    log::info!("event loop finished, exiting");
    Ok(())
}
