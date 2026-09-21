//! Microphone demo: the primary input device drives the show.
//!
//! The host side (this example) captures audio with cpal, computes an RMS
//! level per buffer and feeds the engine through the core contract only:
//! `set_variable("level", ...)` every frame (a blue halo's `scale` binds to
//! it) and `trigger("pop")` when the level spikes above the ambient noise
//! floor (an orange ring pops outward and fades, a keyframed timeline).
//!
//! Press space to fire a pop by hand (useful when no microphone is
//! available or permission was denied), escape to quit. On macOS the first
//! run asks for microphone permission for your terminal.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
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

/// Latest RMS level from the audio callback, stored as f32 bits.
static LEVEL: AtomicU32 = AtomicU32::new(0);

fn store_level(rms: f32) {
    LEVEL.store(rms.to_bits(), Ordering::Relaxed);
}

fn load_level() -> f32 {
    f32::from_bits(LEVEL.load(Ordering::Relaxed))
}

/// Open the default input device and stream RMS levels into [`LEVEL`].
/// Returns `None` (with a warning) when no usable microphone exists; the
/// demo still runs, driven by the space key.
fn start_microphone() -> Option<cpal::Stream> {
    let host = cpal::default_host();
    log::info!("audio host: {:?}", host.id());
    let device = host.default_input_device().or_else(|| {
        log::warn!("no default input device; press space to pop");
        None
    })?;
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("no input config ({e}); press space to pop");
            return None;
        }
    };
    log::info!(
        "listening to {:?} ({} Hz, {} ch, {:?})",
        device
            .description()
            .map_or_else(|_| "?".to_owned(), |d| d.name().to_owned()),
        config.sample_rate(),
        config.channels(),
        config.sample_format()
    );
    let result = match config.sample_format() {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, config.into()),
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, config.into()),
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, config.into()),
        other => {
            log::warn!("unsupported sample format {other:?}; press space to pop");
            return None;
        }
    };
    match result {
        Ok(stream) => match stream.play() {
            Ok(()) => Some(stream),
            Err(e) => {
                log::warn!("could not start stream ({e}); press space to pop");
                None
            }
        },
        Err(e) => {
            log::warn!("could not open stream ({e}); press space to pop");
            None
        }
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            let sum: f32 = data
                .iter()
                .map(|&s| {
                    let v = f32::from_sample(s);
                    v * v
                })
                .sum();
            store_level((sum / data.len().max(1) as f32).sqrt());
        },
        |e| log::error!("audio stream error: {e}"),
        None,
    )
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
    // Keeps the capture stream alive for the app's lifetime.
    _microphone: Option<cpal::Stream>,
    /// Display level, smoothed so the halo does not jitter.
    smoothed: f64,
    /// Slow estimate of the ambient level, for the pop threshold.
    noise_floor: f64,
    last_pop: Instant,
}

impl App {
    fn redraw(&mut self) {
        let Some(state) = &mut self.state else { return };

        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64().min(0.1);
        self.last_frame = now;

        // Feed the mic level in: fast attack, slower release for the halo,
        // and a pop when the raw level jumps well above the noise floor.
        let raw = f64::from(load_level());
        self.smoothed = if raw > self.smoothed {
            raw
        } else {
            self.smoothed * 0.9 + raw * 0.1
        };
        self.noise_floor = (self.noise_floor * 0.995 + raw * 0.005).min(raw.max(0.005));
        self.engine.set_variable("level", self.smoothed);
        let threshold = (self.noise_floor * 4.0).max(0.04);
        if raw > threshold && now.duration_since(self.last_pop).as_secs_f64() > 0.35 {
            log::info!("noise spike: level {raw:.3} over threshold {threshold:.3}, firing \"pop\"");
            self.engine.trigger("pop");
            self.last_pop = now;
        }
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
                        .with_title("cuelight: mic pop")
                        .with_inner_size(LogicalSize::new(w, h)),
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
                        log::info!("space pressed, firing trigger \"pop\"");
                        self.engine.trigger("pop");
                    }
                    _ => {}
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
    log::info!("starting mic_pop example");

    let json = include_str!("shows/mic_pop.json");
    let mut engine = Engine::new();
    engine.load_show(json)?;

    let microphone = start_microphone();

    let now = Instant::now();
    let mut app = App {
        engine,
        context: RenderContext::new(),
        renderers: Vec::new(),
        state: None,
        pending_size: None,
        occluded: false,
        last_frame: now,
        fps: common::Fps::new(),
        images: ImageCache::new(),
        _microphone: microphone,
        smoothed: 0.0,
        noise_floor: 0.01,
        last_pop: now,
    };
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;
    log::info!("event loop finished, exiting");
    Ok(())
}
