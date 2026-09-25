//! Play cuelight shows in a window: load a show folder or a loose show
//! file, list what it can do, drive it from the console, the keyboard or a
//! driver script.
//!
//! ```sh
//! cuelight-player path/to/show[.json|.cuelight] [path/to/driver.json]
//! ```
//!
//! Loading is [`cuelight_loader`]'s: a show folder holds `show.json`, an
//! optional `test-driver.json` (picked up automatically) and `assets/` with
//! images, `fonts/` and `sounds/`; a `.cuelight` file is that folder
//! packed; next to a loose show file the driver is
//! `<show>.test-driver.json`. Without a show the built-in demo plays.
//!
//! The player prints the show's actions (trigger names) and variables: type
//! an action's number or name to fire it, `name=value` to set a variable,
//! `q` to quit. Digit keys in the window fire actions too, `f` or F11
//! switches fullscreen (as does starting with `--fullscreen`), escape
//! leaves fullscreen or quits. Space stops the clock and starts it again,
//! the arrows walk it (a frame, or a second with shift) and home goes back
//! to the start; walking it stops it. All of it keeps working while a
//! driver runs.
//!
//! Images, fonts and sounds a show references but nobody registered are
//! logged as warnings at load and skipped. Sound plays through the default
//! output device unless `--no-audio` is given.
//!
//! The overlay in the corner is the frame rate. Once anything has gone
//! slowly it grows two more counts, which stay for the session: frames
//! that took longer than a frame should, in red, and video frames drawn
//! with an older picture than was due, in amber. An average frame rate
//! absorbs a stall, and video that stutters otherwise looks exactly like
//! a show that renders slowly. A stretch of late video frames is logged
//! as well, saying where in the clip and when in the show it was: as a
//! warning past a few frames, and at `debug` below that, since every
//! clip is a frame or two late as its decoder starts.

use std::io::BufRead;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use cuelight::render::Presenter;
use cuelight::vello;
use cuelight::{Engine, Layer, LayerKind, Value};
use cuelight_audio::{Output, Sound};
use cuelight_loader::{Driver, DriverPlayer, Step};
#[cfg(feature = "video")]
use cuelight_video::{Clip, Decode};
use vello::util::{RenderContext, RenderSurface};
use vello::wgpu;
use wgpu::CurrentSurfaceTexture;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

mod common;

/// A workaround for KDE: on Plasma 6.7 (KWin) with Mesa 26.2 (RADV) a
/// vsynced (FIFO) swapchain leaves a frame waiting up to a second for an
/// image while the window is being resized, freezing the picture. Mailbox
/// never waits, but it does not pace frames either, so it is only used
/// while the size keeps changing and for this long after, with redraws
/// held to the display's refresh rate meanwhile.
///
/// Not needed on COSMIC 1.8 (same machine and driver: smooth without it);
/// GNOME is untested. It is applied on every Wayland compositor that offers
/// mailbox all the same, since it costs nothing where it is not needed and
/// the compositor cannot be told apart reliably. Slow Wayland resizing of
/// Vulkan windows is reported elsewhere too
/// (<https://github.com/glfw/glfw/issues/2493>).
const VSYNC_AFTER_RESIZE: Duration = Duration::from_millis(250);

/// How far the window may be off the show's shape before the player asks
/// for a better size at startup, in logical pixels.
const SHAPE_SLACK: f64 = 4.0;

/// A show of `size` fitted into `room` logical pixels, keeping its shape
/// and never grown beyond its own size.
fn fit_in((room_width, room_height): (f64, f64), [w, h]: [u32; 2]) -> (f64, f64) {
    let (width, height) = (f64::from(w), f64::from(h));
    let fit = (room_width / width).min(room_height / height).min(1.0);
    (width * fit, height * fit)
}

/// How much of `monitor` a window may take, in logical pixels.
fn room_on(monitor: &winit::monitor::MonitorHandle) -> (f64, f64) {
    let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
    (size.width * 0.9, size.height * 0.9)
}

fn fmt_value(value: &Value) -> String {
    match value {
        Value::Text(text) => format!("{text:?}"),
        other => other.to_text(),
    }
}

/// Warn (once, at load) about image, text and audio layers whose pixels,
/// font or sound nobody registered.
fn warn_missing_images(engine: &Engine, layers: &[Layer]) {
    let fonts = &engine.show().expect("show loaded").fonts;
    for layer in layers {
        match &layer.kind {
            LayerKind::Image { image, .. } if engine.image(image).is_none() => {
                log::warn!(
                    "image {image:?} (layer {:?}) is not registered; it will not render",
                    layer.name
                );
            }
            LayerKind::Text { font, .. } => {
                if let Some(style) = fonts.get(font).filter(|s| !engine.has_font(&s.file)) {
                    log::warn!(
                        "font {:?} (layer {:?}) is not registered; it will not render",
                        style.file,
                        layer.name
                    );
                }
            }
            LayerKind::Vector { vector, .. } if engine.vector(vector).is_none() => {
                log::warn!(
                    "vector {vector:?} (layer {:?}) is not registered; it will not render",
                    layer.name
                );
            }
            LayerKind::Audio { sound, .. } => {
                for sound in sound.iter().filter(|s| engine.sound_duration(s).is_none()) {
                    log::warn!(
                        "sound {sound:?} (layer {:?}) is not registered; it will not be heard",
                        layer.name
                    );
                }
            }
            _ => {}
        }
        warn_missing_images(engine, layer.children());
    }
}

fn print_menu(engine: &Engine, actions: &[String]) {
    let show = engine.show().expect("show loaded");
    println!(
        "\nplaying {:?} ({}x{})",
        show.name, show.size[0], show.size[1]
    );
    if actions.is_empty() {
        println!("actions: none declared");
    } else {
        println!("actions:");
        for (i, action) in actions.iter().enumerate() {
            println!("  {}) {action}", i + 1);
        }
    }
    if !show.scenes.is_empty() {
        let scenes: Vec<&str> = show.scenes.iter().map(|s| s.name.as_str()).collect();
        println!(
            "scenes: {} (active: {})",
            scenes.join(", "),
            engine.active_scene().unwrap_or("none")
        );
    }
    if !show.variables.is_empty() {
        println!("variables:");
        for (name, value) in &show.variables {
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

/// The videos of the show, decoded, by the name their layers use.
#[cfg(feature = "video")]
type Videos = std::collections::BTreeMap<String, Clip>;
#[cfg(not(feature = "video"))]
type Videos = ();

struct App {
    engine: Engine,
    /// What the show's video layers draw, when video is compiled in.
    videos: Videos,
    actions: Vec<String>,
    driver: Option<DriverPlayer>,
    /// The show's own driver as written, for seeking: a scrub replays it
    /// from the top rather than rewinding the one that is running.
    script: Option<Driver>,
    /// The clock is stopped: frames still paint, nothing advances.
    paused: bool,
    /// Shift is held, which makes an arrow a second instead of a frame.
    shift: bool,
    /// The sound device, when one could be opened and was wanted.
    audio: Option<Output>,
    console: Receiver<String>,
    context: RenderContext,
    // One vello renderer per wgpu device the context hands out.
    renderers: Vec<Option<vello::Renderer>>,
    state: Option<RenderState>,
    fullscreen: bool,
    /// The window's newest size, waiting for the next redraw.
    pending_size: Option<(u32, u32)>,
    /// Whether the window has been given the show's shape, which happens
    /// once, on the compositor's first answer.
    fitted: bool,
    /// Fastest refresh rate any monitor offers, for pacing redraws while
    /// the window is resized and the display cannot be asked.
    fastest_hertz: f64,
    mailbox_while_resizing: bool,
    /// When the surface last changed size, while it still presents with
    /// mailbox because of that.
    resized_at: Option<Instant>,
    /// When to draw the next frame while mailbox leaves that to us.
    redraw_at: Option<Instant>,
    occluded: bool,
    /// The instant the show's time 0 was, so the clock is read from it
    /// rather than added up frame by frame. `None` until the first
    /// frame, so opening a window and a device is not show time. Moved
    /// deliberately: paused, scrubbed, or after a stall long enough to
    /// be a suspend.
    anchor: Option<Instant>,
    /// Frames that took longer than a frame should, and video frames
    /// drawn older than they should have been: what a frame rate hides,
    /// shown beside it.
    long_frames: u64,
    fps: common::Fps,
    presenter: Presenter,
}

/// A gap between frames past which the show is not caught up with, in
/// seconds: longer than any hitch, a resize or a moment behind another
/// window, and shorter than a machine that went to sleep.
const A_STALL: f64 = 5.0;

/// Where the clock lands this frame.
struct Tick {
    /// The anchor to keep, moved when the show's time was moved for it.
    anchor: Instant,
    /// The instant on the show's clock to land on.
    target: f64,
    /// How much show time this frame covers, for the driver script.
    dt: f64,
    /// Whether the gap was too long to be a frame, so the show goes on
    /// from where it stopped rather than playing through it. Said here
    /// rather than guessed from the numbers, which a frame of no length
    /// looks exactly like.
    stalled: bool,
}

/// Where a show whose clock reads `time` should be at `now`.
///
/// Read from the anchor, never added up: a frame that took a moment is
/// caught up with rather than dropped, since sound plays at the sound
/// card's rate whatever the window is doing and time lost from the show
/// is lost against its own soundtrack for the rest of the session.
///
/// The anchor moves only deliberately. Paused, it rides along under the
/// show so playing carries on from where it stopped. After a gap too
/// long to be a frame, a suspend or a lid, it moves the same way rather
/// than making the show play through everything it missed.
fn tick(anchor: Option<Instant>, now: Instant, time: f64, paused: bool) -> Tick {
    let held = Tick {
        anchor: anchor_for(now, time),
        target: time,
        dt: 0.0,
        stalled: false,
    };
    // Paused, or not started yet: the show is where it is, and the
    // anchor goes under it. A show anchored when the player was built
    // would count window and device setup as show time and open part
    // way in.
    let (Some(anchor), false) = (anchor, paused) else {
        return held;
    };
    let target = now.duration_since(anchor).as_secs_f64();
    let dt = target - time;
    match dt > A_STALL {
        true => Tick {
            stalled: true,
            ..held
        },
        false => Tick {
            anchor,
            target,
            dt: dt.max(0.0),
            stalled: false,
        },
    }
}

/// The instant a show whose clock reads `time` started at.
fn anchor_for(now: Instant, time: f64) -> Instant {
    now.checked_sub(Duration::from_secs_f64(time.max(0.0)))
        .unwrap_or(now)
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

    /// Fill the monitor the window is on, without decorations or pointer,
    /// or go back to a window. Borderless rather than exclusive fullscreen:
    /// it is what Wayland offers, and it leaves the display's mode alone
    /// everywhere else.
    fn set_fullscreen(&mut self, fullscreen: bool) {
        self.fullscreen = fullscreen;
        let Some(state) = &self.state else { return };
        log::info!("fullscreen: {fullscreen}");
        state
            .window
            .set_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
        state.window.set_cursor_visible(!fullscreen);
    }

    /// Give the window the show's shape, once, when the compositor has
    /// answered with the size it grants. The size asked for at creation is
    /// a guess: a client cannot know which monitor its window will land on
    /// before it is mapped (on Wayland there is no primary monitor and
    /// `current_monitor` is still empty here), and a request too large for
    /// that screen comes back clamped in one direction only, leaving a
    /// window taller or wider than its content. The granted size says how
    /// much room there is, so the show's shape goes inside it.
    fn fit_to_window(&mut self) {
        if self.fitted {
            return;
        }
        let Some(window) = self.state.as_ref().map(|state| state.window.clone()) else {
            return;
        };
        // The compositor has answered, so this is the one size the player
        // picks: every later size, a drag or a fullscreen toggle included,
        // is the user's and is left alone.
        self.fitted = true;
        if self.fullscreen {
            return;
        }
        let size = self.engine.show().expect("show loaded").size;
        let now: LogicalSize<f64> = window.inner_size().to_logical(window.scale_factor());
        // What the window has, and no more of the monitor than a window
        // should take where that monitor is known.
        let mut room = (now.width, now.height);
        if let Some(monitor) = window.current_monitor() {
            let (width, height) = room_on(&monitor);
            room = (room.0.min(width), room.1.min(height));
        }
        let (width, height) = fit_in(room, size);
        // Never grown, so this only ever asks for less; a few pixels are
        // the rounding between physical and logical sizes, not a misfit.
        if now.width - width > SHAPE_SLACK || now.height - height > SHAPE_SLACK {
            log::debug!(
                "fitting the window to the show: {width:.0}x{height:.0} logical instead of {:.0}x{:.0}",
                now.width,
                now.height
            );
            let _ = window.request_inner_size(LogicalSize::new(width, height));
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

    /// How far an arrow moves: a frame, or a second with shift held.
    fn step(&self) -> f64 {
        if self.shift {
            1.0
        } else {
            1.0 / 60.0
        }
    }

    /// Put the show `by` seconds from where it is, forwards or backwards,
    /// and stop the clock so it stays there.
    fn scrub(&mut self, by: f64) {
        let to = (self.engine.time() + by).max(0.0);
        self.paused = true;
        self.driver = cuelight_loader::seek(&mut self.engine, self.script.clone(), to, 60.0);
        // Here rather than at the next frame: the show has been moved,
        // and the clock starting again before then would read it from
        // an anchor that belongs to where it was.
        self.anchor = Some(anchor_for(Instant::now(), self.engine.time()));
        log::info!("at {:.3}s", self.engine.time());
    }

    /// Let the driver script apply what is due after `dt` more seconds.
    fn advance_driver(&mut self, dt: f64) {
        let Some(driver) = &mut self.driver else {
            return;
        };
        let was_done = driver.is_done();
        for step in driver.advance(&mut self.engine, dt) {
            match step {
                Step::Trigger { trigger } => log::info!("driver fires action {trigger:?}"),
                Step::Set { set } => {
                    for (name, value) in set {
                        log::info!("driver sets {name:?} = {}", fmt_value(&value));
                    }
                }
                _ => {}
            }
        }
        if driver.is_done() && !was_done {
            log::info!("driver finished");
        }
    }

    /// Hand the engine the frame of every video that should be showing.
    /// This is all a host has to do for video: the engine says what and
    /// how far in, and a frame is an image like any other.
    #[cfg(feature = "video")]
    fn show_video_frames(&mut self) {
        let playing = self.engine.videos().unwrap_or_default();
        // The show's own clock, so a clip that falls behind can say when
        // in the show it did: its position comes round again every loop.
        let show = self.engine.time();
        // A clip nobody is watching stops decoding: a pack names hundreds
        // and plays a few, and a decoder left open costs a process.
        for (name, clip) in &mut self.videos {
            if clip.is_decoding() && !playing.iter().any(|p| p.video == *name) {
                clip.rest();
            }
        }
        for playing in playing {
            let Some(clip) = self.videos.get_mut(&playing.video) else {
                continue;
            };
            let details = clip.details();
            let (width, height) = (details.width, details.height);
            // The show says whether the play loops; a clip decoded as it
            // goes needs telling, so its decoder can run the clip round
            // and round rather than being restarted at every wrap.
            clip.set_looping(playing.looping);
            if let Some(frame) = clip.frame_at(playing.position, Some(show)) {
                let frame = frame.to_vec();
                // Under the key the play reports, not the clip's name:
                // two layers playing one clip are at two positions, and
                // each draws its own picture.
                if let Err(e) = self.engine.set_image(&playing.frame, width, height, frame) {
                    log::warn!("video {:?}: {e}", playing.video);
                }
            }
        }
    }

    #[cfg(not(feature = "video"))]
    fn show_video_frames(&mut self) {}

    /// Video frames drawn with an older picture than was due, over every
    /// clip that has played: a clip decoded as it goes says so rather
    /// than leaving a stutter looking like a slow show.
    #[cfg(feature = "video")]
    fn late_video_frames(&self) -> u64 {
        self.videos.values().map(|clip| clip.kept().late).sum()
    }

    #[cfg(not(feature = "video"))]
    fn late_video_frames(&self) -> u64 {
        0
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        // While we pace the frames ourselves (see the end of this function)
        // a redraw the windowing system asks for, one per resize event, has
        // to wait its turn too.
        if self.redraw_at.is_some_and(|at| now < at) {
            return;
        }
        let Tick {
            anchor,
            target,
            dt,
            stalled,
        } = tick(self.anchor, now, self.engine.time(), self.paused);
        self.anchor = Some(anchor);
        if dt > 0.034 {
            self.long_frames += 1;
            log::debug!("long frame: {:.0} ms since the last one", dt * 1000.0);
        }
        if stalled {
            log::info!("a stall longer than {A_STALL:.0}s: the show goes on from where it stopped");
        }
        self.advance_driver(dt);
        self.engine.advance_to(target);
        for event in self.engine.drain_events() {
            log::info!("show event: {event:?}");
        }
        self.show_video_frames();
        let late = self.late_video_frames();
        let Some(state) = &mut self.state else { return };
        if let Some(audio) = &self.audio {
            match self.engine.voices() {
                Ok(voices) => audio.apply(&voices),
                Err(e) => log::warn!("voices: {e}"),
            }
        }

        if let Some((width, height)) = self.pending_size.take() {
            let config = &mut state.surface.config;
            if (width, height) != (config.width, config.height) {
                let started = Instant::now();
                if self.mailbox_while_resizing {
                    config.present_mode = wgpu::PresentMode::Mailbox;
                    self.resized_at = Some(started);
                }
                self.context
                    .resize_surface(&mut state.surface, width, height);
                log::debug!(
                    "surface resized to {width}x{height} physical in {:.1} ms",
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
        if self
            .resized_at
            .is_some_and(|at| at.elapsed() > VSYNC_AFTER_RESIZE)
        {
            self.resized_at = None;
            self.context
                .set_present_mode(&mut state.surface, wgpu::PresentMode::AutoVsync);
        }
        let surface = &state.surface;
        let (sw, sh) = (surface.config.width, surface.config.height);
        let device_handle = &self.context.devices[surface.dev_id];
        let renderer = self.renderers[surface.dev_id]
            .as_mut()
            .expect("renderer for surface device");
        // The presenter fits the show into the window and applies its
        // output mode; the player only adds its overlay on top.
        let presented = self
            .presenter
            .present(
                &self.engine,
                &device_handle.device,
                &device_handle.queue,
                renderer,
                [sw, sh],
            )
            .expect("present show");
        let mut frame = presented.scene;
        self.fps.tick();
        // The rate, then what it hides: frames that took too long in
        // red, and video frames drawn late in amber. Neither is shown
        // until there is one.
        let counts: Vec<(u64, [u8; 3])> = match (self.long_frames, late) {
            (0, 0) => Vec::new(),
            (long, late) => vec![(long, [255, 80, 80]), (late, [255, 190, 60])],
        };
        self.fps
            .draw(&mut frame, state.window.scale_factor(), &counts);
        renderer
            .render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                &frame,
                &surface.target_view,
                &vello::RenderParams {
                    base_color: presented.base_color,
                    width: sw,
                    height: sh,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .expect("vello render");

        // Blit the intermediate target to the window surface and present.
        let acquiring = Instant::now();
        let acquired = surface.surface.get_current_texture();
        let waited = acquiring.elapsed().as_secs_f64() * 1000.0;
        if waited > 20.0 {
            let outcome = match &acquired {
                CurrentSurfaceTexture::Success(_) => "success",
                CurrentSurfaceTexture::Suboptimal(_) => "suboptimal",
                CurrentSurfaceTexture::Outdated => "outdated",
                CurrentSurfaceTexture::Lost => "lost",
                CurrentSurfaceTexture::Timeout => "timeout",
                CurrentSurfaceTexture::Occluded => "occluded",
                CurrentSurfaceTexture::Validation => "validation",
            };
            log::debug!("waited {waited:.0} ms for the surface texture: {outcome}");
        }
        let surface_texture = match acquired {
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
        let presenting = Instant::now();
        surface_texture.present();
        let waited = presenting.elapsed().as_secs_f64() * 1000.0;
        // Two frames of a 60 Hz display queue up behind vsync as a matter
        // of course.
        if waited > 50.0 {
            log::debug!("present took {waited:.0} ms");
        }

        if self.resized_at.is_some() {
            // Mailbox does not pace anything: without this a drag draws
            // thousands of frames per second. Come back at the display's
            // pace instead.
            // Wayland leaves `current_monitor` empty, so the fastest
            // display stands in: under mailbox an extra frame is discarded,
            // while too slow a pace makes the drag itself look choppy.
            let hertz = state
                .window
                .current_monitor()
                .and_then(|monitor| monitor.refresh_rate_millihertz())
                .map_or(self.fastest_hertz, |millihertz| {
                    f64::from(millihertz) / 1000.0
                });
            // Counted from this frame's start, so drawing time is not added
            // on top of every interval.
            self.redraw_at = Some(now + Duration::from_secs_f64(1.0 / hertz));
        } else {
            state.window.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let [w, h] = self.engine.show().expect("show loaded").size;
        // The show at its own size, but never more than most of the screen:
        // a large show on a scaled display would otherwise open as big as
        // the compositor allows, which looks like fullscreen. Which screen
        // that is can only be guessed here (Wayland has no primary monitor
        // and tells a client nothing about placement before the window is
        // mapped), so `fit_to_monitor` corrects it once the window is up.
        let guess = event_loop
            .primary_monitor()
            .or_else(|| event_loop.available_monitors().next());
        let room = guess
            .as_ref()
            .map_or((f64::INFINITY, f64::INFINITY), room_on);
        let (width, height) = fit_in(room, [w, h]);
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("cuelight: player")
                        .with_inner_size(LogicalSize::new(width, height)),
                )
                .expect("create window"),
        );
        common::log_window_info(&window);
        self.fastest_hertz = event_loop
            .available_monitors()
            .filter_map(|monitor| monitor.refresh_rate_millihertz())
            .map(|millihertz| f64::from(millihertz) / 1000.0)
            .fold(60.0_f64, f64::max);
        log::debug!("fastest display: {:.0} Hz", self.fastest_hertz);
        let size = window.inner_size();
        let surface = pollster::block_on(self.context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::AutoVsync,
        ))
        .expect("create surface");
        common::log_adapter(&self.context, surface.dev_id);
        // See `VSYNC_AFTER_RESIZE`.
        self.mailbox_while_resizing = common::is_wayland(&window) && {
            let adapter = self.context.devices[surface.dev_id].adapter();
            let modes = surface.surface.get_capabilities(adapter).present_modes;
            modes.contains(&wgpu::PresentMode::Mailbox)
        };
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
        if self.fullscreen {
            self.set_fullscreen(true);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                log::info!("close requested, exiting");
                event_loop.exit();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) if self.fullscreen => self.set_fullscreen(false),
                    Key::Named(NamedKey::Escape) => {
                        log::info!("escape pressed, exiting");
                        event_loop.exit();
                    }
                    Key::Named(NamedKey::F11) => self.set_fullscreen(!self.fullscreen),
                    Key::Character(c) if c.eq_ignore_ascii_case("f") => {
                        self.set_fullscreen(!self.fullscreen);
                    }
                    // Space stops the clock; the arrows walk it. A show is
                    // a function of its inputs and the clock, so going
                    // back is replaying to there, not rewinding.
                    Key::Named(NamedKey::Space) => {
                        self.paused = !self.paused;
                        // The clock starts again from where the show is,
                        // whether or not a frame has been drawn since it
                        // was put there.
                        self.anchor = Some(anchor_for(Instant::now(), self.engine.time()));
                        log::info!(
                            "{} at {:.3}s",
                            if self.paused { "paused" } else { "playing" },
                            self.engine.time()
                        );
                    }
                    Key::Named(NamedKey::ArrowLeft) => self.scrub(-self.step()),
                    Key::Named(NamedKey::ArrowRight) => self.scrub(self.step()),
                    Key::Named(NamedKey::Home) => {
                        let to = -self.engine.time();
                        self.scrub(to);
                    }
                    Key::Character(c) => {
                        if let Ok(digit) = c.as_str().parse::<usize>() {
                            self.fire_action(digit.wrapping_sub(1));
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.shift = modifiers.state().shift_key();
            }
            // Only noted here: a drag can deliver several sizes per frame,
            // and each reconfiguration of the surface costs milliseconds.
            // The next redraw applies the last one.
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    self.pending_size = Some((size.width, size.height));
                    self.fit_to_window();
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
        if let Some(at) = self.redraw_at {
            if Instant::now() < at {
                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                return;
            }
            self.redraw_at = None;
            if let Some(state) = &self.state {
                state.window.request_redraw();
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

/// Play a cuelight show in a window.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Show folder, packed show (.cuelight) or loose show file; the
    /// built-in demo when omitted.
    show: Option<std::path::PathBuf>,
    /// Driver script to play instead of the one found next to the show.
    driver: Option<std::path::PathBuf>,
    /// Do not play any driver script.
    #[arg(long, conflicts_with = "driver")]
    no_driver: bool,
    /// Start fullscreen on the monitor the window opens on.
    #[arg(long)]
    fullscreen: bool,
    /// Do not open a sound device; the show plays silently.
    #[arg(long)]
    no_audio: bool,
}

const DEMO_SHOW: &str = include_str!("../demo/show.json");
const DEMO_DRIVER: &str = include_str!("../demo/test-driver.json");

/// Decode the show's videos and tell the engine how long and how big they
/// are. Without the feature there is nothing to decode and video layers
/// simply show nothing.
#[cfg(feature = "video")]
fn decode_videos(engine: &mut Engine, paths: &[std::path::PathBuf]) -> Videos {
    let mut videos = Videos::new();
    // Frames are held in memory here, so a clip is decoded no larger than
    // the canvas can show: a backglass video is often far bigger than the
    // show it plays in, and full size would cost gigabytes.
    let canvas = engine.show().map(|show| show.size);
    let how = Decode {
        size: canvas,
        ..Decode::default()
    };
    // Measuring a clip means running a probe, so a show that names
    // hundreds waits on hundreds of them. They do not depend on each
    // other, so they run together.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let opened: Vec<(String, Result<Clip, String>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(path) = paths.get(i) else {
                            return mine;
                        };
                        let name = path
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        mine.push((i, name, Clip::open(path, Some(how))));
                    }
                })
            })
            .collect();
        let mut all: Vec<_> = workers
            .into_iter()
            .filter_map(|worker| worker.join().ok())
            .flatten()
            .collect();
        // Back into the show's own order, so the log reads like the folder.
        all.sort_by_key(|(i, ..)| *i);
        all.into_iter()
            .map(|(_, name, clip)| (name, clip))
            .collect()
    });
    for (name, opened) in opened {
        let started = Instant::now();
        match opened {
            Ok(clip) => {
                let details = clip.details();
                log::debug!(
                    "video {name:?}: {:.1}s at {}x{}, measured in {:.0} ms",
                    details.duration,
                    details.width,
                    details.height,
                    started.elapsed().as_secs_f64() * 1000.0
                );
                if let Err(e) = engine.set_video(&name, details.duration, details.size()) {
                    log::warn!("video {name:?}: {e}");
                    continue;
                }
                videos.insert(name, clip);
            }
            Err(e) => log::warn!("skipping video {name:?}: {e}"),
        }
    }
    videos
}

/// The rate a clip's soundtrack is decoded at. The mixer resamples, and a
/// `Sound` carries its own rate, so this need not match the device: it
/// only has to be decided before one is open, since whether any clip has
/// sound is what decides whether to open one at all.
#[cfg(feature = "video")]
const SOUNDTRACK_RATE: u32 = 48_000;

/// Tell the engine about the soundtrack of every clip that has one, under
/// the video's name, which is how it is told a clip can be heard. Hands
/// back the samples for a mixer, which is not open yet: whether any clip
/// has sound is what decides whether to open one.
///
/// Decoded together, like the clips are measured together: a folder of a
/// couple of hundred clips is a couple of hundred runs of ffmpeg, and
/// they do not depend on each other.
#[cfg(feature = "video")]
fn decode_video_sound(
    engine: &mut Engine,
    paths: &[std::path::PathBuf],
) -> Vec<(String, Arc<Sound>)> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut found: Vec<(usize, String, Vec<f32>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(path) = paths.get(i) else {
                            return mine;
                        };
                        let name = path
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        match cuelight_video::soundtrack(path, SOUNDTRACK_RATE) {
                            Ok(Some(samples)) => mine.push((i, name, samples)),
                            Ok(None) => {}
                            Err(e) => log::warn!("soundtrack of {name:?}: {e}"),
                        }
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .filter_map(|worker| worker.join().ok())
            .flatten()
            .collect()
    });
    // Back into the show's own order, so the log reads like the folder.
    found.sort_by_key(|(i, ..)| *i);
    let mut decoded = Vec::new();
    for (_, name, samples) in found {
        let sound = Sound {
            rate: SOUNDTRACK_RATE,
            channels: 2,
            samples,
        };
        let duration = sound.duration();
        if let Err(e) = engine.set_sound(&name, duration) {
            log::warn!("soundtrack of {name:?}: {e}");
            continue;
        }
        log::debug!("video {name:?}: {duration:.1}s of sound");
        decoded.push((name, Arc::new(sound)));
    }
    decoded
}

#[cfg(not(feature = "video"))]
fn decode_video_sound(_: &mut Engine, _: &[std::path::PathBuf]) -> Vec<(String, Arc<Sound>)> {
    Vec::new()
}

#[cfg(not(feature = "video"))]
fn decode_videos(_: &mut Engine, paths: &[std::path::PathBuf]) -> Videos {
    if !paths.is_empty() {
        log::warn!(
            "{} video(s) in the show, but this player was built without video support",
            paths.len()
        );
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let mut engine = Engine::new();
    #[allow(unused_assignments)]
    let mut videos = Videos::default();
    // Sound is optional: without a device the show still plays. The
    // device is looked for once the show is loaded, since a show with no
    // audio layer has no use for one.
    let mut audio = None;
    // `anyway` is for a clip that turned out to carry a soundtrack: whether
    // a video is heard is not something the show's document can say, so
    // `has_sound` does not count video layers and the host says so itself.
    let want_audio = |engine: &Engine, audio: &mut Option<Output>, anyway: bool| {
        if cli.no_audio || audio.is_some() {
            return;
        }
        if !anyway && !engine.show().is_some_and(cuelight::Show::has_sound) {
            log::debug!("no audio layers in this show; not looking for a sound device");
            return;
        }
        *audio = Output::open().map_err(|e| log::warn!("no sound: {e}")).ok();
    };
    let driver = match &cli.show {
        Some(path) => {
            let loaded = cuelight_loader::load(&mut engine, path)?;
            log::info!(
                "loaded {:?}: {} image(s), {} font(s), {} sound(s)",
                loaded.show,
                loaded.images.len(),
                loaded.fonts.len(),
                loaded.sounds.len()
            );
            for skipped in &loaded.skipped {
                log::warn!("skipping asset {skipped:?}: no decoder for this format");
            }
            videos = decode_videos(&mut engine, &loaded.videos);
            // Nothing will play them, and decoding a folder of clips'
            // audio is neither quick nor small.
            let clip_sound = if cli.no_audio {
                Vec::new()
            } else {
                decode_video_sound(&mut engine, &loaded.videos)
            };
            want_audio(&engine, &mut audio, !clip_sound.is_empty());
            if let Some(audio) = &audio {
                for (name, sound) in clip_sound {
                    audio.set_sound(&name, sound);
                }
            }
            for file in &loaded.sounds {
                match Sound::decode(&file.extension, &file.bytes) {
                    Ok(sound) => {
                        engine.set_sound(&file.name, sound.duration())?;
                        if let Some(audio) = &audio {
                            audio.set_sound(&file.name, Arc::new(sound));
                        }
                    }
                    Err(e) => log::warn!("skipping sound {:?}: {e}", file.name),
                }
            }
            match &cli.driver {
                Some(path) => Some(Driver::from_file(path)?),
                None => loaded.driver,
            }
        }
        None => {
            log::info!("no show given, playing the built-in demo");
            engine.load_show(DEMO_SHOW)?;
            want_audio(&engine, &mut audio, false);
            Some(Driver::from_json(DEMO_DRIVER)?)
        }
    };
    for field in engine.load_warnings() {
        log::warn!("show field {field:?} is not understood and was ignored");
    }
    let script = driver.filter(|_| !cli.no_driver).inspect(|driver| {
        log::info!(
            "driver: {} steps{}",
            driver.steps.len(),
            if driver.looping { ", looping" } else { "" }
        );
    });
    let driver = script.clone().map(DriverPlayer::new);

    let show = engine.show().expect("show loaded");
    for layers in show.layer_trees() {
        warn_missing_images(&engine, layers);
    }
    let actions: Vec<String> = show.triggers().into_iter().collect();
    print_menu(&engine, &actions);

    let (tx, rx) = std::sync::mpsc::channel();
    spawn_console_reader(tx);

    let mut app = App {
        engine,
        videos,
        actions,
        script: script.clone(),
        paused: false,
        shift: false,
        driver,
        audio,
        console: rx,
        context: RenderContext::new(),
        renderers: Vec::new(),
        state: None,
        fullscreen: cli.fullscreen,
        pending_size: None,
        fitted: false,
        fastest_hertz: 60.0,
        mailbox_while_resizing: false,
        resized_at: None,
        redraw_at: None,
        occluded: false,
        anchor: None,
        long_frames: 0,
        fps: common::Fps::new(),
        presenter: Presenter::new(),
    };
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;
    log::info!("event loop finished, exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{anchor_for, tick, Duration, Instant, A_STALL};

    /// Frames of the given lengths, from a show at 0, playing: where the
    /// show's clock ends up, and where the wall clock does.
    fn played(frames: &[f64]) -> (f64, f64) {
        let start = Instant::now();
        let mut anchor = start;
        let (mut now, mut time) = (start, 0.0);
        for frame in frames {
            now += Duration::from_secs_f64(*frame);
            let t = tick(Some(anchor), now, time, false);
            (anchor, time) = (t.anchor, t.target);
        }
        (time, now.duration_since(start).as_secs_f64())
    }

    #[test]
    fn a_frame_that_took_a_moment_is_caught_up_with() {
        // A show run at 60 fps with four hitches in it, one of them a
        // sixth of a second: it is still where the wall clock says, not
        // a quarter of a second behind it for the rest of the session.
        let mut frames = vec![1.0 / 60.0; 600];
        for (at, long) in [(100, 0.162), (200, 0.120), (300, 0.104), (400, 0.135)] {
            frames[at] = long;
        }
        let (played, wall) = played(&frames);
        assert!((played - wall).abs() < 1e-9, "{played} against {wall}");
        // A clock that added the frames up, with anything past a tenth
        // of a second dropped, would be an eighth of a second behind by
        // now and stay there: 62 ms of it from the worst frame alone.
        let added: f64 = frames.iter().map(|f| f.min(0.1)).sum();
        assert!(wall - added > 0.12, "{added} against {wall}");
    }

    #[test]
    fn a_thousand_frames_do_not_add_up_to_a_drift() {
        // A frame length that does not divide the second: added up it
        // drifts, read from an anchor it cannot.
        let frames = vec![1.0 / 1024.0 + 1e-7; 1000];
        let (played, wall) = played(&frames);
        assert!((played - wall).abs() < 1e-9, "{played} against {wall}");
    }

    #[test]
    fn paused_holds_the_clock_and_plays_on_from_there() {
        let start = Instant::now();
        let mut anchor = start;
        let mut time = 2.0;
        // Ten seconds of frames while paused: the show stays at 2.
        for frame in 1..=10 {
            let now = start + Duration::from_secs(frame);
            let t = tick(Some(anchor), now, time, true);
            (anchor, time) = (t.anchor, t.target);
            assert_eq!(time, 2.0);
        }
        // And playing again carries on from there rather than jumping.
        let now = start + Duration::from_secs_f64(10.5);
        let t = tick(Some(anchor), now, time, false);
        assert!((t.target - 2.5).abs() < 1e-9, "{}", t.target);
        assert!((t.dt - 0.5).abs() < 1e-9, "{}", t.dt);
    }

    #[test]
    fn the_show_starts_at_its_first_frame_not_when_the_player_was_built() {
        // Opening a window and a device takes long enough to skip the
        // opening of a show, so the clock starts where the show is.
        let built = Instant::now();
        let first = built + Duration::from_secs_f64(2.5);
        let t = tick(None, first, 0.0, false);
        assert_eq!((t.target, t.dt, t.stalled), (0.0, 0.0, false));
        // And the frame after it is an ordinary frame.
        let next = tick(
            Some(t.anchor),
            first + Duration::from_secs_f64(1.0 / 60.0),
            t.target,
            false,
        );
        assert!((next.dt - 1.0 / 60.0).abs() < 1e-9, "{}", next.dt);
    }

    #[test]
    fn a_frame_of_no_length_is_not_a_stall() {
        // Two frames landing on the same instant look exactly like a
        // re-anchor from the outside, which is why the tick says which
        // it was rather than leaving it to be guessed.
        let now = Instant::now();
        let t = tick(Some(anchor_for(now, 3.0)), now, 3.0, false);
        assert_eq!((t.target, t.dt, t.stalled), (3.0, 0.0, false));
    }

    #[test]
    fn a_stall_too_long_to_be_a_frame_starts_again_where_it_stopped() {
        let start = Instant::now();
        let time = 7.0;
        let anchor = start - Duration::from_secs_f64(time);
        let gap = A_STALL + 60.0;
        let t = tick(
            Some(anchor),
            start + Duration::from_secs_f64(gap),
            time,
            false,
        );
        assert_eq!((t.target, t.dt), (7.0, 0.0), "the minute is not played");
        assert!(t.stalled, "and it says so, rather than being inferred");
        // And the anchor came with it, so the next frame is a frame.
        let next = tick(
            Some(t.anchor),
            start + Duration::from_secs_f64(gap + 1.0 / 60.0),
            t.target,
            false,
        );
        assert!((next.dt - 1.0 / 60.0).abs() < 1e-9, "{}", next.dt);
    }
}
