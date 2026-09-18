//! Generic scene player: load any scene file, list what it can do, drive
//! it interactively or from a driver file.
//!
//! ```sh
//! cargo run --example player -- path/to/scene.json [path/to/driver.json]
//! ```
//!
//! With no arguments the bundled minigolf scene plays. The player inspects
//! the model and prints its actions (trigger names) and variables to the
//! console: type an action's number or name to fire it, `name=value` to
//! set a variable, `q` to quit. Digit keys in the window fire actions too,
//! escape quits.
//!
//! A driver file scripts the same commands with delays, standing in for a
//! live host (see `examples/drivers/`):
//!
//! ```json
//! {
//!   "loop": true,
//!   "steps": [
//!     { "set": { "score": 0 } },
//!     { "wait": 0.5 },
//!     { "trigger": "go" }
//!   ]
//! }
//! ```
//!
//! Console and keyboard input keep working while a driver runs.
//!
//! The player registers no images, so scenes referencing host-provided
//! images (like the slideshow's) render without them; each missing image
//! is logged as a warning at load.

use std::collections::BTreeSet;
use std::io::BufRead;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cuelight::render::{background_color, build_vello_scene, ImageCache};
use cuelight::vello;
use cuelight::{Engine, Layer, LayerKind, Value};
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

/// A scripted command sequence with delays, standing in for a live host.
#[derive(Debug, Clone, serde::Deserialize)]
struct Driver {
    #[serde(default, rename = "loop")]
    looping: bool,
    #[serde(default)]
    steps: Vec<Step>,
}

/// One driver step: exactly one of `wait` (seconds), `trigger` (fire an
/// action) or `set` (variable assignments).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum Step {
    Wait {
        wait: f64,
    },
    Trigger {
        trigger: String,
    },
    Set {
        set: std::collections::BTreeMap<String, Value>,
    },
}

/// Playback state for a loaded [`Driver`].
struct DriverState {
    driver: Driver,
    index: usize,
    wait_left: f64,
    done: bool,
}

impl DriverState {
    fn new(driver: Driver) -> Self {
        Self {
            driver,
            index: 0,
            wait_left: 0.0,
            done: false,
        }
    }
}

fn fmt_value(value: &Value) -> String {
    match value {
        Value::Text(text) => format!("{text:?}"),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
    }
}

/// Collect every trigger name declared by timelines in the layer tree.
fn collect_actions(layers: &[Layer], out: &mut BTreeSet<String>) {
    for layer in layers {
        for timeline in &layer.timelines {
            if let Some(trigger) = &timeline.trigger {
                out.insert(trigger.clone());
            }
        }
        if let LayerKind::Group { children } = &layer.kind {
            collect_actions(children, out);
        }
    }
}

/// Warn (once, at load) about image layers whose pixels nobody registered.
fn warn_missing_images(engine: &Engine, layers: &[Layer]) {
    for layer in layers {
        match &layer.kind {
            LayerKind::Image { image, .. } if engine.image(image).is_none() => {
                log::warn!(
                    "image {image:?} (layer {:?}) is not registered; it will not render",
                    layer.name
                );
            }
            LayerKind::Group { children } => warn_missing_images(engine, children),
            _ => {}
        }
    }
}

fn print_menu(engine: &Engine, actions: &[String]) {
    let scene = engine.scene().expect("scene loaded");
    println!(
        "\nplaying {:?} ({}x{})",
        scene.name, scene.size[0], scene.size[1]
    );
    if actions.is_empty() {
        println!("actions: none declared");
    } else {
        println!("actions:");
        for (i, action) in actions.iter().enumerate() {
            println!("  {}) {action}", i + 1);
        }
    }
    if !scene.variables.is_empty() {
        println!("variables:");
        for (name, value) in &scene.variables {
            println!("  {name} = {}", fmt_value(value));
        }
    }
    println!("type an action number or name, name=value to set a variable, q to quit");
}

/// Read console lines on a background thread; the event loop polls them.
fn spawn_console_reader(tx: Sender<String>) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let line = line.trim().to_owned();
            if !line.is_empty() && tx.send(line).is_err() {
                break;
            }
        }
    });
}

struct RenderState {
    window: Arc<Window>,
    surface: RenderSurface<'static>,
}

struct App {
    engine: Engine,
    actions: Vec<String>,
    driver: Option<DriverState>,
    console: Receiver<String>,
    context: RenderContext,
    // One vello renderer per wgpu device the context hands out.
    renderers: Vec<Option<vello::Renderer>>,
    state: Option<RenderState>,
    occluded: bool,
    last_frame: Instant,
    fps: common::Fps,
    images: ImageCache,
}

impl App {
    fn fire_action(&mut self, index: usize) {
        match self.actions.get(index) {
            Some(action) => {
                log::info!("firing action {:?}", action);
                let action = action.clone();
                self.engine.trigger(&action);
            }
            None => log::warn!("no action number {}", index + 1),
        }
    }

    /// Apply one console command; returns false when the player should quit.
    fn handle_command(&mut self, command: &str) -> bool {
        match command {
            "q" | "quit" | "exit" => {
                log::info!("quit requested on console");
                return false;
            }
            _ => {}
        }
        if let Some((name, value)) = command.split_once('=') {
            let (name, value) = (name.trim(), value.trim());
            match value.parse::<f64>() {
                Ok(number) => {
                    log::info!("setting variable {name:?} = {number}");
                    self.engine.set_variable(name, number);
                }
                Err(_) => log::warn!("{value:?} is not a number"),
            }
        } else if let Ok(number) = command.parse::<usize>() {
            self.fire_action(number.wrapping_sub(1));
        } else {
            log::info!("firing action {command:?}");
            self.engine.trigger(command);
        }
        true
    }

    /// Advance the driver playhead by `dt`: waits consume time, commands
    /// execute the moment their wait is over.
    fn advance_driver(&mut self, mut dt: f64) {
        let Some(d) = &mut self.driver else { return };
        if d.done {
            return;
        }
        let mut wraps = 0;
        loop {
            if d.wait_left > 0.0 {
                if dt < d.wait_left {
                    d.wait_left -= dt;
                    return;
                }
                dt -= d.wait_left;
                d.wait_left = 0.0;
            }
            if d.index >= d.driver.steps.len() {
                if !d.driver.looping || d.driver.steps.is_empty() {
                    log::info!("driver finished");
                    d.done = true;
                    return;
                }
                wraps += 1;
                if wraps > 1 {
                    log::warn!("looping driver has no wait steps; stopping it");
                    d.done = true;
                    return;
                }
                log::debug!("driver loops, restarting");
                d.index = 0;
            }
            let step = d.driver.steps[d.index].clone();
            d.index += 1;
            match step {
                Step::Wait { wait } => d.wait_left = wait.max(0.0),
                Step::Trigger { trigger } => {
                    log::info!("driver fires action {trigger:?}");
                    self.engine.trigger(&trigger);
                }
                Step::Set { set } => {
                    for (name, value) in set {
                        log::info!("driver sets {name:?} = {}", fmt_value(&value));
                        self.engine.set_variable(&name, value);
                    }
                }
            }
        }
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64().min(0.1);
        self.last_frame = now;
        self.advance_driver(dt);
        let Some(state) = &self.state else { return };
        self.engine.advance_frame(dt);

        // Fit the scene into the window: uniform scale, centered.
        let [scene_w, scene_h] = self.engine.scene().expect("scene loaded").size;
        let surface = &state.surface;
        let (sw, sh) = (surface.config.width, surface.config.height);
        let scale = (f64::from(sw) / f64::from(scene_w)).min(f64::from(sh) / f64::from(scene_h));
        let tx = (f64::from(sw) - f64::from(scene_w) * scale) / 2.0;
        let ty = (f64::from(sh) - f64::from(scene_h) * scale) / 2.0;
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
        let [w, h] = self.engine.scene().expect("scene loaded").size;
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("cuelight: player")
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
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        log::info!("escape pressed, exiting");
                        event_loop.exit();
                    }
                    Key::Character(c) => {
                        if let Ok(digit) = c.as_str().parse::<usize>() {
                            self.fire_action(digit.wrapping_sub(1));
                        }
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
        while let Ok(command) = self.console.try_recv() {
            if !self.handle_command(&command) {
                event_loop.exit();
                return;
            }
        }
        event_loop.set_control_flow(if self.occluded {
            // No redraws while hidden: wake periodically so ctrl-c and
            // console commands still land promptly.
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
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/examples/scenes/minigolf.json").to_owned()
    });
    log::info!("starting player with scene {path:?}");
    let json =
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read scene {path:?}: {e}"))?;

    let mut engine = Engine::new();
    engine
        .load_scene(&json)
        .map_err(|e| format!("cannot load scene {path:?}: {e}"))?;

    let driver = match std::env::args().nth(2) {
        Some(driver_path) => {
            let json = std::fs::read_to_string(&driver_path)
                .map_err(|e| format!("cannot read driver {driver_path:?}: {e}"))?;
            let driver: Driver = serde_json::from_str(&json)
                .map_err(|e| format!("cannot load driver {driver_path:?}: {e}"))?;
            log::info!(
                "driving with {driver_path:?}: {} steps{}",
                driver.steps.len(),
                if driver.looping { ", looping" } else { "" }
            );
            Some(DriverState::new(driver))
        }
        None => None,
    };

    let mut actions = BTreeSet::new();
    let scene_layers = engine.scene().expect("scene loaded").layers.clone();
    collect_actions(&scene_layers, &mut actions);
    warn_missing_images(&engine, &scene_layers);
    let actions: Vec<String> = actions.into_iter().collect();
    print_menu(&engine, &actions);

    let (tx, rx) = std::sync::mpsc::channel();
    spawn_console_reader(tx);

    let mut app = App {
        engine,
        actions,
        driver,
        console: rx,
        context: RenderContext::new(),
        renderers: Vec::new(),
        state: None,
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
