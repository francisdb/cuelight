//! Play `cuelight` shows in a browser: a show folder fetched over HTTP,
//! rendered into a canvas with WebGPU, driven from JavaScript.
//!
//! ```js
//! import init, { CuelightPlayer } from "./pkg/cuelight_web.js";
//!
//! await init();
//! const player = await CuelightPlayer.create(canvas, "shows/beacon/");
//! player.actions();            // ["go", ...]
//! player.trigger("go");
//! player.set("score", 1200);
//! player.onEvent((event) => console.log(event));
//! ```
//!
//! The show folder needs a `manifest.json` (the `cuelight-manifest` tool of
//! `cuelight-loader` writes it): a browser cannot list a directory. A
//! packed show (`shows/beacon.cuelight`) needs nothing: it is one fetch,
//! unpacked here. Either way its `test-driver.json`, when there is one,
//! starts playing right away.
//!
//! The page sizes the canvas with CSS; the player keeps the canvas's pixel
//! size in step with it and fits the show inside. Frames follow
//! `requestAnimationFrame`, so a hidden tab costs nothing and the show
//! picks up where it was.
//!
//! `demo/build.sh` shows the build: `cargo build` for `wasm32`, then
//! `wasm-bindgen --target web`. It is a plain build; for production see the
//! note in that script on shrinking the download.
//!
//! Sound goes through WebAudio: the folder's `assets/sounds/` are decoded
//! by the browser and the engine's voice list drives buffer sources and
//! gain nodes (the `audio` module). Browsers keep audio silent until the page has
//! been clicked or typed into; the player resumes its context on the first
//! such gesture, and `player.audioRunning` says whether it has.
//! `player.audioEnabled = false` silences a show and keeps it silent
//! through later gestures; `true` lets it through again.
//!
//! WebGPU only: without it `CuelightPlayer.create` rejects with a message
//! saying so. The crate is empty on targets other than `wasm32`.

#![cfg(target_arch = "wasm32")]

mod audio;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use cuelight::render::Presenter;
use cuelight::vello;
use cuelight::{Engine, Event, Value};
use cuelight_loader::{Driver, DriverPlayer, Manifest, MANIFEST_FILE};
use vello::util::{RenderContext, RenderSurface};
use vello::wgpu;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::HtmlCanvasElement;

/// A gap between frames past which the show is not caught up with, in
/// seconds: longer than any hitch, and shorter than a tab left in the
/// background. A tab coming back continues from where it stopped
/// instead of playing through everything it missed.
const A_STALL: f64 = 5.0;

type FrameCallback = Closure<dyn FnMut(f64)>;
/// An event name and the handler listening for it on the document.
type GestureListener = (String, Closure<dyn FnMut()>);

fn error(message: impl AsRef<str>) -> JsValue {
    js_sys::Error::new(message.as_ref()).into()
}

fn window() -> Result<web_sys::Window, JsValue> {
    web_sys::window().ok_or_else(|| error("no window: not running in a page"))
}

async fn fetch(url: &str) -> Result<Vec<u8>, JsValue> {
    let response: web_sys::Response = JsFuture::from(window()?.fetch_with_str(url))
        .await
        .map_err(|_| error(format!("{url}: request failed")))?
        .dyn_into()?;
    if !response.ok() {
        return Err(error(format!("{url}: HTTP {}", response.status())));
    }
    let buffer = JsFuture::from(response.array_buffer()?).await?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

/// Fetch a show: a packed `.cuelight` file, unpacked here, or the folder
/// at `base` (ending in `/`): its manifest, then the files it lists, all
/// at once.
async fn fetch_show(base: &str) -> Result<BTreeMap<String, Vec<u8>>, JsValue> {
    if base.ends_with(".cuelight") {
        let bytes = fetch(base).await?;
        return cuelight_loader::unpack(&bytes).map_err(|e| error(format!("{base}: {e}")));
    }
    let manifest_url = format!("{base}{MANIFEST_FILE}");
    let manifest = String::from_utf8(fetch(&manifest_url).await?)
        .map_err(|e| e.to_string())
        .and_then(|json| Manifest::from_json(&json))
        .map_err(|e| error(format!("{manifest_url}: {e}")))?;
    let fetches = manifest.files.iter().map(|file| async move {
        let bytes = fetch(&format!("{base}{file}")).await?;
        Ok::<_, JsValue>((file.clone(), bytes))
    });
    futures_util::future::try_join_all(fetches)
        .await
        .map(|files| files.into_iter().collect())
}

fn to_js(value: &Value) -> JsValue {
    match value {
        Value::Bool(b) => JsValue::from_bool(*b),
        Value::Number(n) => JsValue::from_f64(*n),
        Value::Text(text) => JsValue::from_str(text),
        _ => JsValue::UNDEFINED,
    }
}

fn from_js(value: &JsValue) -> Result<Value, JsValue> {
    if let Some(b) = value.as_bool() {
        Ok(Value::Bool(b))
    } else if let Some(n) = value.as_f64() {
        Ok(Value::Number(n))
    } else if let Some(text) = value.as_string() {
        Ok(Value::Text(text))
    } else {
        Err(error("a variable takes a boolean, a number or a string"))
    }
}

fn event_to_js(event: &Event) -> JsValue {
    let object = js_sys::Object::new();
    let set = |key: &str, value: &str| {
        let _ = js_sys::Reflect::set(&object, &key.into(), &value.into());
    };
    match event {
        Event::Trigger(name) => {
            set("type", "trigger");
            set("name", name);
        }
        _ => set("type", "unknown"),
    }
    object.into()
}

struct Inner {
    engine: Engine,
    script: Option<Driver>,
    driver: Option<DriverPlayer>,
    driver_playing: bool,
    /// The clock is stopped: frames still paint, nothing advances.
    paused: bool,
    warnings: Vec<String>,
    canvas: HtmlCanvasElement,
    context: RenderContext,
    surface: RenderSurface<'static>,
    renderer: vello::Renderer,
    presenter: Presenter,
    /// The page timestamp the show's time 0 was at, so the clock is read
    /// from it rather than added up frame by frame. Moved deliberately:
    /// paused, seeked, or after a gap long enough to be a background
    /// tab.
    anchor_ms: Option<f64>,
    on_event: Option<js_sys::Function>,
    pending_frame: Option<i32>,
    /// The page's sound, when the browser gave us an audio context.
    audio: Option<audio::WebAudio>,
}

impl Inner {
    /// Pixel size the canvas should have for its CSS size on this screen.
    fn wanted_size(&self) -> (u32, u32) {
        let ratio = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        let max = self.context.devices[self.surface.dev_id]
            .device
            .limits()
            .max_texture_dimension_2d;
        let pixels = |css: i32| ((f64::from(css) * ratio).round() as u32).clamp(1, max);
        (
            pixels(self.canvas.client_width()),
            pixels(self.canvas.client_height()),
        )
    }

    /// Keep the canvas's pixel size in step with its CSS size.
    fn sync_size(&mut self) {
        let (width, height) = self.wanted_size();
        if (width, height) == (self.surface.config.width, self.surface.config.height)
            && (width, height) == (self.canvas.width(), self.canvas.height())
        {
            return;
        }
        let css = (self.canvas.client_width(), self.canvas.client_height());
        self.canvas.set_width(width);
        self.canvas.set_height(height);
        // A canvas without a CSS size takes its layout size from the pixel
        // size just set, which would grow it every frame on a high-density
        // screen: pin it to the size it had.
        if (self.canvas.client_width(), self.canvas.client_height()) != css {
            let style = self.canvas.style();
            let _ = style.set_property("width", &format!("{}px", css.0));
            let _ = style.set_property("height", &format!("{}px", css.1));
        }
        self.context
            .resize_surface(&mut self.surface, width, height);
    }

    /// Advance the show to `now_ms` and draw it. Returns the show's events
    /// for the caller to deliver once the player is no longer borrowed.
    fn frame(&mut self, now_ms: f64) -> Result<Vec<Event>, String> {
        // Paused stops the clock, not the painting: a resize, a seek or a
        // variable set from the page still shows. The anchor rides along
        // under the time the show is stopped at, so playing carries on
        // from there.
        let time = self.engine.time();
        if self.paused || self.anchor_ms.is_none() {
            self.anchor_ms = Some(now_ms - time * 1000.0);
        }
        let anchor = self.anchor_ms.unwrap_or(now_ms);
        let mut target = (now_ms - anchor) / 1000.0;
        let mut dt = (target - time).max(0.0);
        // A frame that took a moment is caught up with, since sound
        // plays on whatever the page is doing and a show that dropped
        // that time would be out against its own soundtrack for good. A
        // gap long enough to be a background tab is not a frame to catch
        // up with: the show goes on from where it stopped.
        if dt > A_STALL {
            self.anchor_ms = Some(now_ms - time * 1000.0);
            (target, dt) = (time, 0.0);
        }
        if self.driver_playing && !self.paused {
            if let Some(driver) = &mut self.driver {
                driver.advance(&mut self.engine, dt);
            }
        }
        self.engine.advance_to(target);
        let events = self.engine.drain_events();
        if let Some(audio) = &mut self.audio {
            if let Ok(voices) = self.engine.voices() {
                audio.apply(&voices);
            }
        }

        self.sync_size();
        let surface = &self.surface;
        let (width, height) = (surface.config.width, surface.config.height);
        let handle = &self.context.devices[surface.dev_id];
        let presented = self
            .presenter
            .present(
                &self.engine,
                &handle.device,
                &handle.queue,
                &mut self.renderer,
                [width, height],
            )
            .map_err(|e| e.to_string())?;
        self.renderer
            .render_to_texture(
                &handle.device,
                &handle.queue,
                &presented.scene,
                &surface.target_view,
                &vello::RenderParams {
                    base_color: presented.base_color,
                    width,
                    height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|e| e.to_string())?;
        let texture = match surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            // Skip this frame; the next one retries.
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.context.configure_surface(surface);
                return Ok(events);
            }
            _ => return Ok(events),
        };
        let mut encoder = handle
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cuelight-surface-blit"),
            });
        surface.blitter.copy(
            &handle.device,
            &mut encoder,
            &surface.target_view,
            &texture
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        );
        handle.queue.submit([encoder.finish()]);
        texture.present();
        Ok(events)
    }
}

/// A show playing in a canvas. Call `free()` to stop it.
#[wasm_bindgen]
pub struct CuelightPlayer {
    inner: Rc<RefCell<Inner>>,
    // Owns the frame callback; the callback itself only holds weak
    // references, so dropping the player ends the loop.
    _frame: Rc<RefCell<Option<FrameCallback>>>,
    // The gesture listeners that wake the audio context, removed on drop.
    _gestures: Vec<GestureListener>,
}

#[wasm_bindgen]
impl CuelightPlayer {
    /// Fetch the show folder at `url` and start playing it in `canvas`.
    pub async fn create(canvas: HtmlCanvasElement, url: String) -> Result<CuelightPlayer, JsValue> {
        console_error_panic_hook::set_once();
        let gpu = js_sys::Reflect::get(&window()?.navigator(), &"gpu".into())?;
        if gpu.is_undefined() {
            return Err(error(
                "this browser has no WebGPU, which the cuelight player needs",
            ));
        }

        let base = if url.is_empty() || url.ends_with('/') || url.ends_with(".cuelight") {
            url
        } else {
            format!("{url}/")
        };
        let files = fetch_show(&base).await?;
        let mut engine = Engine::new();
        let loaded = cuelight_loader::load_from_memory(&mut engine, &files)
            .map_err(|e| error(format!("{base}: {e}")))?;
        let mut warnings: Vec<String> = engine
            .load_warnings()
            .iter()
            .map(|field| format!("show field {field:?} is not understood and was ignored"))
            .collect();
        warnings.extend(
            loaded
                .skipped
                .iter()
                .map(|file| format!("asset {file:?} was skipped: no decoder for this format")),
        );
        // Sounds: decoded by the browser, registered by duration.
        let mut audio = match audio::WebAudio::new() {
            Ok(audio) => Some(audio),
            Err(e) => {
                warnings.push(format!("no sound: {}", js_error_text(&e)));
                None
            }
        };
        if let Some(audio) = &mut audio {
            for file in &loaded.sounds {
                match audio.decode(&file.name, &file.bytes).await {
                    Ok(duration) => {
                        if let Err(e) = engine.set_sound(&file.name, duration) {
                            warnings.push(format!("sound {:?}: {e}", file.name));
                        }
                    }
                    Err(e) => warnings.push(format!(
                        "sound {:?} could not be decoded: {}",
                        file.name,
                        js_error_text(&e)
                    )),
                }
            }
        }
        for warning in &warnings {
            web_sys::console::warn_1(&format!("cuelight: {warning}").into());
        }

        let mut context = RenderContext::new();
        let surface = context
            .create_surface(
                wgpu::SurfaceTarget::Canvas(canvas.clone()),
                canvas.width().max(1),
                canvas.height().max(1),
                wgpu::PresentMode::AutoVsync,
            )
            .await
            .map_err(|e| error(format!("no WebGPU surface for the canvas: {e}")))?;
        let renderer = vello::Renderer::new(
            &context.devices[surface.dev_id].device,
            vello::RendererOptions::default(),
        )
        .map_err(|e| error(format!("renderer: {e}")))?;

        let inner = Rc::new(RefCell::new(Inner {
            paused: false,
            engine,
            driver: loaded.driver.clone().map(DriverPlayer::new),
            driver_playing: loaded.driver.is_some(),
            script: loaded.driver,
            warnings,
            canvas,
            context,
            surface,
            renderer,
            presenter: Presenter::new(),
            anchor_ms: None,
            on_event: None,
            pending_frame: None,
            audio,
        }));
        let frame = start_frames(&inner)?;
        let gestures = listen_for_gestures(&inner)?;
        Ok(CuelightPlayer {
            inner,
            _frame: frame,
            _gestures: gestures,
        })
    }

    /// Whether sound is playing, or the browser still waits for a click or
    /// a key press on the page before it lets audio through.
    #[wasm_bindgen(getter, js_name = audioRunning)]
    pub fn audio_running(&self) -> bool {
        self.inner
            .borrow()
            .audio
            .as_ref()
            .is_some_and(audio::WebAudio::running)
    }

    /// Ask the browser to let sound through; only works from within a
    /// click or key handler, which the player already installs on the
    /// page, so pages rarely need this.
    #[wasm_bindgen(js_name = resumeAudio)]
    pub fn resume_audio(&self) {
        if let Some(audio) = &self.inner.borrow().audio {
            audio.resume();
        }
    }

    /// Whether the page wants sound: on by default. Off silences the show
    /// (every voice stops and its audio context is suspended) and stays off
    /// through clicks and key presses; on lets sound through again, with
    /// plays picking up at their current positions. On a page the browser
    /// has not seen a gesture on yet, sound still waits for one:
    /// `audioRunning` is the truth. `false` when the browser gave no audio
    /// context.
    #[wasm_bindgen(getter, js_name = audioEnabled)]
    pub fn audio_enabled(&self) -> bool {
        self.inner
            .borrow()
            .audio
            .as_ref()
            .is_some_and(audio::WebAudio::enabled)
    }

    #[wasm_bindgen(setter, js_name = audioEnabled)]
    pub fn set_audio_enabled(&self, enabled: bool) {
        if let Some(audio) = &mut self.inner.borrow_mut().audio {
            audio.set_enabled(enabled);
        }
    }

    /// Fire a trigger.
    pub fn trigger(&self, name: &str) {
        self.inner.borrow_mut().engine.trigger(name);
    }

    /// Set a variable to a boolean, a number or a string.
    pub fn set(&self, name: &str, value: JsValue) -> Result<(), JsValue> {
        let value = from_js(&value)?;
        self.inner.borrow_mut().engine.set_variable(name, value);
        Ok(())
    }

    /// A variable's current value, `undefined` when the show has none by
    /// that name.
    pub fn get(&self, name: &str) -> JsValue {
        let inner = self.inner.borrow();
        inner
            .engine
            .variable(name)
            .map_or(JsValue::UNDEFINED, to_js)
    }

    /// The triggers the show listens to, sorted.
    pub fn actions(&self) -> Vec<String> {
        let inner = self.inner.borrow();
        let show = inner.engine.show().expect("show loaded");
        show.triggers().into_iter().collect()
    }

    /// The show's variables with their current values, as an object.
    pub fn variables(&self) -> JsValue {
        let inner = self.inner.borrow();
        let object = js_sys::Object::new();
        for name in inner.engine.show().expect("show loaded").variables.keys() {
            let value = inner
                .engine
                .variable(name)
                .map_or(JsValue::UNDEFINED, to_js);
            let _ = js_sys::Reflect::set(&object, &name.into(), &value);
        }
        object.into()
    }

    /// The show's scene names, in document order.
    pub fn scenes(&self) -> Vec<String> {
        let inner = self.inner.borrow();
        let show = inner.engine.show().expect("show loaded");
        show.scenes.iter().map(|s| s.name.clone()).collect()
    }

    #[wasm_bindgen(getter, js_name = activeScene)]
    pub fn active_scene(&self) -> Option<String> {
        self.inner.borrow().engine.active_scene().map(str::to_owned)
    }

    /// The show's canvas size, e.g. for the page to set an aspect ratio.
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.inner.borrow().engine.show().expect("show loaded").size[0]
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.inner.borrow().engine.show().expect("show loaded").size[1]
    }

    /// What loading complained about; also logged to the console.
    pub fn warnings(&self) -> Vec<String> {
        self.inner.borrow().warnings.clone()
    }

    /// Whether the show folder came with a driver script.
    #[wasm_bindgen(getter, js_name = hasDriver)]
    pub fn has_driver(&self) -> bool {
        self.inner.borrow().script.is_some()
    }

    /// Whether the driver script is playing: false once paused or finished.
    #[wasm_bindgen(getter, js_name = driverPlaying)]
    pub fn driver_playing(&self) -> bool {
        let inner = self.inner.borrow();
        inner.driver_playing && inner.driver.as_ref().is_some_and(|d| !d.is_done())
    }

    /// Continue the driver script, from the top when it had finished.
    #[wasm_bindgen(js_name = driverPlay)]
    pub fn driver_play(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.driver.as_ref().is_some_and(DriverPlayer::is_done) {
            inner.driver = inner.script.clone().map(DriverPlayer::new);
        }
        inner.driver_playing = inner.driver.is_some();
    }

    #[wasm_bindgen(js_name = driverPause)]
    pub fn driver_pause(&self) {
        self.inner.borrow_mut().driver_playing = false;
    }

    /// Stop the clock. Frames keep painting, so a resize or a seek still
    /// shows, but nothing advances and the driver stops with it.
    pub fn pause(&self) {
        self.inner.borrow_mut().paused = true;
    }

    /// Start the clock again from where it stopped.
    pub fn resume(&self) {
        self.inner.borrow_mut().paused = false;
    }

    #[wasm_bindgen(getter)]
    pub fn paused(&self) -> bool {
        self.inner.borrow().paused
    }

    /// Seconds of show time that have passed.
    #[wasm_bindgen(getter)]
    pub fn time(&self) -> f64 {
        self.inner.borrow().engine.time()
    }

    /// Put the show at `seconds`, forwards or backwards.
    ///
    /// The show is restarted and advanced to that moment in steps of
    /// `1 / fps`, replaying the driver script on the way, because a
    /// show's state is a function of its inputs and the clock rather than
    /// anything the engine remembers. A show of a minute costs a couple
    /// of milliseconds, so a page may call this as a scrub bar moves.
    ///
    /// Use the rate the show plays at: a chain of timelines linked by
    /// `on_end` lands on frame boundaries, so another rate can put them a
    /// frame or two apart.
    pub fn seek(&self, seconds: f64, fps: Option<f64>) {
        let mut inner = self.inner.borrow_mut();
        let script = inner.script.clone();
        let fps = fps.filter(|f| f.is_finite() && *f > 0.0).unwrap_or(60.0);
        let Inner { engine, .. } = &mut *inner;
        let played = cuelight_loader::seek(engine, script, seconds.max(0.0), fps);
        inner.driver = played;
        // The clock is read from the anchor, and the show is somewhere
        // else now: the next frame works out where it starts from.
        inner.anchor_ms = None;
    }

    /// Call `callback` with every event the show raises, as
    /// `{ type: "trigger", name }`; `null` stops it.
    #[wasm_bindgen(js_name = onEvent)]
    pub fn on_event(&self, callback: Option<js_sys::Function>) {
        self.inner.borrow_mut().on_event = callback;
    }
}

impl Drop for CuelightPlayer {
    fn drop(&mut self) {
        let pending = self.inner.borrow_mut().pending_frame.take();
        if let (Some(id), Some(window)) = (pending, web_sys::window()) {
            let _ = window.cancel_animation_frame(id);
        }
        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
            for (event, closure) in &self._gestures {
                let _ = document
                    .remove_event_listener_with_callback(event, closure.as_ref().unchecked_ref());
            }
        }
    }
}

fn js_error_text(e: &JsValue) -> String {
    e.as_string()
        .or_else(|| {
            e.dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message()))
        })
        .unwrap_or_else(|| format!("{e:?}"))
}

/// Resume the audio context on the first click or key press anywhere on
/// the page: browsers only let sound through after a gesture, and only
/// when asked from within its handler.
fn listen_for_gestures(inner: &Rc<RefCell<Inner>>) -> Result<Vec<GestureListener>, JsValue> {
    let document = window()?.document().ok_or_else(|| error("no document"))?;
    let mut listeners = Vec::new();
    for event in ["pointerdown", "keydown"] {
        let weak = Rc::downgrade(inner);
        let closure = Closure::new(move || {
            if let Some(inner) = weak.upgrade() {
                if let Some(audio) = &inner.borrow().audio {
                    audio.resume();
                }
            }
        });
        document.add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())?;
        listeners.push((event.to_owned(), closure));
    }
    Ok(listeners)
}

/// Start the `requestAnimationFrame` loop. The returned cell owns the
/// callback; the loop ends when it is dropped.
fn start_frames(inner: &Rc<RefCell<Inner>>) -> Result<Rc<RefCell<Option<FrameCallback>>>, JsValue> {
    fn request(window: &web_sys::Window, inner: &RefCell<Inner>, callback: &FrameCallback) {
        let id = window.request_animation_frame(callback.as_ref().unchecked_ref());
        inner.borrow_mut().pending_frame = id.ok();
    }

    let cell: Rc<RefCell<Option<FrameCallback>>> = Rc::new(RefCell::new(None));
    let (weak_cell, weak_inner) = (Rc::downgrade(&cell), Rc::downgrade(inner));
    let callback = Closure::new(move |now_ms: f64| {
        let (Some(cell), Some(inner)): (Option<Rc<_>>, Option<Rc<RefCell<Inner>>>) =
            (Weak::upgrade(&weak_cell), Weak::upgrade(&weak_inner))
        else {
            return;
        };
        // The borrow ends before any callback runs: a callback may well
        // call back into the player.
        let (result, on_event) = {
            let mut inner = inner.borrow_mut();
            inner.pending_frame = None;
            (inner.frame(now_ms), inner.on_event.clone())
        };
        match result {
            Ok(events) => {
                if let Some(on_event) = on_event {
                    for event in &events {
                        let _ = on_event.call1(&JsValue::NULL, &event_to_js(event));
                    }
                }
            }
            // The loop stops: a frame that failed will fail again.
            Err(e) => {
                web_sys::console::error_1(&format!("cuelight: {e}").into());
                return;
            }
        }
        let callback = cell.borrow();
        if let (Some(window), Some(callback)) = (web_sys::window(), callback.as_ref()) {
            request(&window, &inner, callback);
        }
    });
    request(&window()?, inner, &callback);
    *cell.borrow_mut() = Some(callback);
    Ok(cell)
}
