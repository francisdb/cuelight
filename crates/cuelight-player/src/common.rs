//! Shared helpers for the windowed examples.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use cuelight::vello;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

static CTRL_C: AtomicBool = AtomicBool::new(false);

/// Initialize env_logger: `info` for the examples, quieter for the noisy
/// GPU crates. Override with `RUST_LOG` (e.g. `RUST_LOG=debug` to see
/// per-resize and per-trigger detail).
pub fn init_logging() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "info,wgpu_core=warn,wgpu_hal=warn,naga=warn,calloop=warn,polling=warn",
    ))
    .init();
}

/// Install a Ctrl-C handler that flags [`ctrl_c_pressed`]. The winit event
/// loop does not observe SIGINT itself, so the continuously redrawing
/// examples poll the flag each frame and exit the loop cleanly.
pub fn install_ctrl_c_handler() {
    let _ = ctrlc::set_handler(|| {
        log::info!("ctrl-c received, shutting down");
        CTRL_C.store(true, Ordering::Relaxed);
    });
}

pub fn ctrl_c_pressed() -> bool {
    CTRL_C.load(Ordering::Relaxed)
}

/// Log the windowing situation: physical size, hidpi scale factor and the
/// platform windowing backend (on linux: x11 vs wayland).
pub fn log_window_info(window: &winit::window::Window) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let backend = match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Wayland(_)) => "wayland",
        Ok(RawWindowHandle::Xlib(_)) | Ok(RawWindowHandle::Xcb(_)) => "x11",
        Ok(RawWindowHandle::AppKit(_)) => "appkit (macos)",
        Ok(RawWindowHandle::Win32(_)) => "win32",
        _ => "unknown",
    };
    let size = window.inner_size();
    log::info!(
        "window: {}x{} physical, scale factor {}, windowing backend {}",
        size.width,
        size.height,
        window.scale_factor(),
        backend
    );
}

/// Whether the window lives on a Wayland compositor.
pub fn is_wayland(window: &winit::window::Window) -> bool {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    matches!(
        window.window_handle().map(|h| h.as_raw()),
        Ok(RawWindowHandle::Wayland(_))
    )
}

/// Log which GPU and wgpu backend the surface's device runs on.
pub fn log_adapter(context: &vello::util::RenderContext, dev_id: usize) {
    let info = context.devices[dev_id].adapter().get_info();
    log::info!(
        "render backend: {:?} on {:?} ({:?})",
        info.backend,
        info.name,
        info.device_type
    );
}

/// Segment masks for 0-9, bits a..g (top, top-right, bottom-right, bottom,
/// bottom-left, top-left, middle).
const SEGMENTS: [u8; 10] = [0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F];

/// Frames-per-second estimate, drawn as a seven-segment overlay (the crate
/// has no text rendering, so the examples bring their own digits).
pub struct Fps {
    frames: u32,
    since: Instant,
    value: f64,
}

impl Fps {
    pub fn new() -> Self {
        Self {
            frames: 0,
            since: Instant::now(),
            value: 0.0,
        }
    }

    /// Count one presented frame; the estimate refreshes every half second.
    pub fn tick(&mut self) {
        self.frames += 1;
        let elapsed = self.since.elapsed().as_secs_f64();
        if elapsed >= 0.5 {
            self.value = f64::from(self.frames) / elapsed;
            log::trace!("{:.0} frames per second", self.value);
            self.frames = 0;
            self.since = Instant::now();
        }
    }

    /// Draw the counter in the top-left corner, in surface coordinates,
    /// with `counts` beside it in their own colours: what a frame rate
    /// on its own hides, since an average absorbs a stall.
    ///
    /// `scale` is the window's hidpi factor so the overlay keeps a
    /// constant logical size.
    pub fn draw(&self, scene: &mut vello::Scene, scale: f64, counts: &[(u64, [u8; 3])]) {
        let white = [255, 255, 255];
        let mut groups = vec![((self.value.round() as u64).min(9999), white)];
        groups.extend(counts.iter().map(|&(n, color)| (n.min(9999), color)));
        let digits = |mut n: u64| {
            let mut out = Vec::new();
            loop {
                out.push((n % 10) as usize);
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            out.reverse();
            out
        };
        let groups: Vec<(Vec<usize>, [u8; 3])> = groups
            .into_iter()
            .map(|(n, color)| (digits(n), color))
            .collect();

        let (dw, dh, t, gap) = (5.0 * scale, 9.0 * scale, 1.0 * scale, 2.0 * scale);
        let (margin, pad) = (2.0 * scale, 3.0 * scale);
        let between = 5.0 * scale;
        let group_w = |digits: &Vec<usize>| digits.len() as f64 * (dw + gap) - gap;
        let total_w: f64 = groups.iter().map(|(d, _)| group_w(d)).sum::<f64>()
            + between * (groups.len() - 1) as f64;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0, 0, 0, 160),
            None,
            &RoundedRect::new(
                margin,
                margin,
                margin + total_w + 2.0 * pad,
                margin + dh + 2.0 * pad,
                2.0 * scale,
            ),
        );
        let mut x = margin + pad;
        for (digits, [r, g, b]) in &groups {
            let color = Color::from_rgba8(*r, *g, *b, 230);
            for (i, &d) in digits.iter().enumerate() {
                let at = x + i as f64 * (dw + gap);
                draw_digit(scene, d, at, margin + pad, dw, dh, t, color);
            }
            x += group_w(digits) + between;
        }
    }
}

impl Default for Fps {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_digit(
    scene: &mut vello::Scene,
    digit: usize,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    t: f64,
    color: Color,
) {
    let mask = SEGMENTS[digit];
    let mid = y + h / 2.0;
    let segments = [
        Rect::new(x + t, y, x + w - t, y + t),
        Rect::new(x + w - t, y + t * 0.5, x + w, mid - t * 0.5),
        Rect::new(x + w - t, mid + t * 0.5, x + w, y + h - t * 0.5),
        Rect::new(x + t, y + h - t, x + w - t, y + h),
        Rect::new(x, mid + t * 0.5, x + t, y + h - t * 0.5),
        Rect::new(x, y + t * 0.5, x + t, mid - t * 0.5),
        Rect::new(x + t, mid - t * 0.5, x + w - t, mid + t * 0.5),
    ];
    for (bit, rect) in segments.iter().enumerate() {
        if mask & (1 << bit) != 0 {
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, rect);
        }
    }
}
