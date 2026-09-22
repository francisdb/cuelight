use crate::font::{BitmapFont, Rgba, StyledFont};
use crate::lru::ByteLru;
use crate::model::{
    parse_color, Align, Binding, Blend, DigitDisplay, Justify, Layer, LayerKind, Output, Pass,
    Property, Retrigger, Scaling, Shape, Sheet, Show, Timeline, FORMAT,
};
use crate::output::OutputColor;
use crate::path::{self, PathElement};
use crate::segments;
use crate::value::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("no show loaded")]
    NoShow,
    #[error("invalid show: {0}")]
    InvalidShow(String),
    #[error("show format {found} is newer than this engine supports (up to {supported})")]
    UnsupportedFormat { found: u64, supported: u32 },
    #[error("invalid color literal {0:?}")]
    InvalidColor(String),
    #[error("invalid image: {0}")]
    InvalidImage(String),
    #[error("invalid font: {0}")]
    InvalidFont(String),
    #[error("invalid sound: {0}")]
    InvalidSound(String),
}

/// A host-provided raster image: tightly packed RGBA8 pixels (straight,
/// non-premultiplied alpha), kept in memory and shared with renderers.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major RGBA8.
    pub pixels: Arc<[u8]>,
    revision: u64,
}

/// The next revision for anything a renderer caches by it. One sequence
/// for the whole process, so a cache shared between engines (a host
/// playing several shows, tests sharing a renderer) cannot mistake one
/// engine's asset for another's.
fn next_revision() -> u64 {
    static REVISION: AtomicU64 = AtomicU64::new(0);
    REVISION.fetch_add(1, Ordering::Relaxed) + 1
}

impl ImageData {
    /// Unique to this upload for as long as the process runs. Lets
    /// renderers cache GPU resources per upload instead of comparing
    /// pixels.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Wrap engine-generated pixels with a fresh revision.
    fn generated(rgba: Rgba) -> Self {
        Self {
            width: rgba.width,
            height: rgba.height,
            pixels: rgba.pixels.into(),
            revision: next_revision(),
        }
    }
}

/// A map of the plane, `(x, y)` to `(a x + c y + e, b x + d y + f)`, its
/// coefficients in the order `[a, b, c, d, e, f]` that SVG and kurbo use.
/// The draw list places items with one where a rotation or an uneven
/// scale is involved; see [`ResolvedLayer::transform`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform(pub [f64; 6]);

impl Transform {
    pub const IDENTITY: Transform = Transform([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn translate(x: f64, y: f64) -> Self {
        Transform([1.0, 0.0, 0.0, 1.0, x, y])
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Transform([sx, 0.0, 0.0, sy, 0.0, 0.0])
    }

    /// Rotation by `degrees`, clockwise on a canvas whose y grows down.
    pub fn rotate(degrees: f64) -> Self {
        let (s, c) = degrees.to_radians().sin_cos();
        Transform([c, s, -s, c, 0.0, 0.0])
    }

    /// This map applied after `inner`.
    pub fn then(self, inner: Transform) -> Transform {
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = inner.0;
        Transform([
            a * a2 + c * b2,
            b * a2 + d * b2,
            a * c2 + c * d2,
            b * c2 + d * d2,
            a * e2 + c * f2 + e,
            b * e2 + d * f2 + f,
        ])
    }

    pub fn apply(self, [x, y]: [f64; 2]) -> [f64; 2] {
        let [a, b, c, d, e, f] = self.0;
        [a * x + c * y + e, b * x + d * y + f]
    }

    /// `(scale, x, y)` when this is only a positive uniform scale and a
    /// translation, which the draw list bakes into its coordinates.
    pub fn plain(self) -> Option<(f64, f64, f64)> {
        let [a, b, c, d, e, f] = self.0;
        (b == 0.0 && c == 0.0 && a == d && a > 0.0).then_some((a, e, f))
    }
}

/// A binding with a transition or a debounce: layer tree, layer path,
/// binding index.
type TransitionSite = (Root, Vec<usize>, usize);

/// A debounced binding's input: the value that reached the property, and
/// the newer one waiting to have held long enough.
#[derive(Debug, Clone)]
struct Settling {
    settled: Value,
    candidate: Value,
    /// Engine time the candidate first appeared.
    since: f64,
}

/// A bound value on its way from `start` to `target` since engine time
/// `started`. The value in between is computed from this, never stepped,
/// so it does not depend on the frame rate.
#[derive(Debug, Clone, Copy)]
struct Change {
    start: f64,
    target: f64,
    started: f64,
}

/// Which layer tree a layer path is rooted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Root {
    /// The show's own, always present layers.
    Show,
    /// The layers of the scene at this index.
    Scene(usize),
}

/// A host-provided outline font file (TrueType / OpenType), shared with
/// renderers.
#[derive(Debug, Clone, PartialEq)]
pub struct FontData {
    /// The font file's bytes.
    pub data: Arc<[u8]>,
    revision: u64,
}

impl FontData {
    /// Unique to this registration for as long as the process runs, so
    /// renderers can cache their font object per upload instead of
    /// comparing bytes.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// Host-provided vector artwork (an SVG the loader converted): paths in
/// its own units, drawn by vector layers.
#[derive(Debug, Clone, PartialEq)]
pub struct Vector {
    /// The artwork's natural size, what a vector layer without `size`
    /// draws at.
    pub width: f64,
    pub height: f64,
    /// In paint order.
    pub paths: Vec<VectorPath>,
}

/// One path of a [`Vector`]: its geometry and how it is painted.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorPath {
    pub elements: Vec<PathElement>,
    /// Fill color as RGBA bytes; `None` for an outline only.
    pub fill: Option<[u8; 4]>,
    /// Stroke color and width, in the artwork's units.
    pub stroke: Option<([u8; 4], f64)>,
}

/// One glyph of a [`ResolvedShape::GlyphRun`]: its id in the font and the
/// position of its origin on the baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedGlyph {
    pub id: u32,
    pub x: f64,
    pub y: f64,
}

/// A host-registered bitmap font: its description and page pixels.
#[derive(Debug)]
struct RegisteredFont {
    font: BitmapFont,
    pages: Vec<Rgba>,
}

/// How a text layer gets drawn, see `Engine::text_draw`.
enum TextDraw {
    Bitmap(Arc<TextRaster>),
    #[cfg_attr(not(feature = "outline-fonts"), allow(dead_code))]
    Glyphs {
        font: FontData,
        size: f64,
        /// Relative to the layer's box.
        glyphs: Vec<PlacedGlyph>,
        container: [f64; 2],
    },
}

/// Rasterized text, placed relative to its layer's box.
#[derive(Debug)]
struct TextRaster {
    image: ImageData,
    offset: [i32; 2],
    /// The box the text was laid out in.
    container: [f64; 2],
}

/// Caches for text rendering, filled lazily while resolving layers.
#[derive(Debug)]
struct TextCache {
    /// Styled fonts by style name.
    styled: HashMap<String, Arc<StyledFont>>,
    /// Rasters by style, text, box and alignment; `None` for text that
    /// draws nothing.
    rasters: ByteLru<String, Option<Arc<TextRaster>>>,
}

impl Default for TextCache {
    fn default() -> Self {
        Self {
            styled: HashMap::new(),
            rasters: ByteLru::new(MAX_RASTER_BYTES),
        }
    }
}

/// Budget for cached text rasters. Every distinct string is its own
/// bitmap, so a changing number in a large font adds up quickly; the least
/// recently used rasters go first, which keeps static labels cached.
const MAX_RASTER_BYTES: usize = 32 * 1024 * 1024;

/// A running timeline instance.
#[derive(Debug, Clone)]
struct Playhead {
    root: Root,
    /// Path to the owning layer within its root's layer tree.
    layer_path: Vec<usize>,
    /// Timeline index within that layer.
    timeline: usize,
    /// Seconds since the delay ended: negative while delayed.
    time: f64,
}

/// One play of an audio layer, from its trigger until it ends or is
/// stopped.
#[derive(Debug, Clone)]
struct Sounding {
    root: Root,
    layer_path: Vec<usize>,
    /// Engine-unique, so a backend can tell one play from the next.
    id: u64,
    /// Engine time it was triggered at; the delay counts from here.
    started: f64,
}

/// A sound that should be heard now, as [`Engine::voices`] reports it: the
/// audio twin of a [`ResolvedLayer`]. Plain data, so a backend can be fed
/// and tested without an engine.
#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    /// Identifies one play for as long as it lasts; never reused within an
    /// engine. A backend starts a sound when an id appears and stops it
    /// when the id is gone.
    pub id: u64,
    /// Name of the audio layer.
    pub layer: String,
    /// The sound, as registered with [`Engine::set_sound`].
    pub sound: String,
    /// Seconds into the sound, wrapped for loops and repeats. A backend
    /// starts a new voice here and resyncs one that has drifted away
    /// from it (a seek).
    pub position: f64,
    /// Effective loudness: the layer's gain times every group's above it.
    pub gain: f64,
    /// Whether the sound plays on from its end.
    pub looping: bool,
    /// The bus the layer names, if any.
    pub bus: Option<String>,
}

/// Something the show did that hosts may want to react to; collect them
/// with [`Engine::drain_events`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// The show fired this trigger itself (a timeline's or a sound's
    /// `on_end`). Triggers the host fires are not echoed back.
    Trigger(String),
}

/// Events kept while the host does not drain them; the oldest are dropped
/// beyond this so an uninterested host costs nothing.
const MAX_PENDING_EVENTS: usize = 256;

/// The engine: owns the loaded show and all runtime state.
///
/// Hosts drive it through the four core calls (`load_show`, `set_variable`,
/// `trigger`, `advance_frame`) and read the result back either as
/// [`resolved_layers`](Engine::resolved_layers) (a flattened draw list, no
/// GPU involved) or through the `render` feature's rasterizer.
#[derive(Debug, Default)]
pub struct Engine {
    show: Option<Show>,
    variables: BTreeMap<String, Value>,
    images: BTreeMap<String, ImageData>,
    vectors: BTreeMap<String, Vector>,
    fonts: BTreeMap<String, RegisteredFont>,
    outline_fonts: BTreeMap<String, FontData>,
    text_cache: Mutex<TextCache>,
    /// Registered sounds and their durations in seconds.
    sounds: BTreeMap<String, f64>,
    playing: Vec<Playhead>,
    sounding: Vec<Sounding>,
    /// Id for the next play; ids are never reused.
    next_voice: u64,
    /// Every binding with a transition, found once at load.
    transition_sites: Vec<TransitionSite>,
    /// The change each of them is making; absent until its first frame,
    /// so a property starts at its value instead of easing in.
    transitions: HashMap<TransitionSite, Change>,
    /// Every binding with a debounce, found once at load, and what each
    /// has settled on.
    debounce_sites: Vec<TransitionSite>,
    debounced: HashMap<TransitionSite, Settling>,
    /// Every reel row, found once at load, and where each of its cells is
    /// on its ring: one record per cell, in cell order.
    reel_sites: Vec<(Root, Vec<usize>)>,
    reels: HashMap<(Root, Vec<usize>), Vec<Change>>,
    active_scene: Option<usize>,
    events: std::collections::VecDeque<Event>,
    load_warnings: Vec<String>,
    time: f64,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a show from its JSON description, replacing any current show
    /// and resetting all runtime state. The first scene (if any) becomes
    /// active; autoplay timelines start at 0.
    pub fn load_show(&mut self, json: &str) -> Result<(), Error> {
        let invalid = |e: serde_json::Error| Error::InvalidShow(e.to_string());
        let raw: serde_json::Value = serde_json::from_str(json).map_err(invalid)?;
        // Refuse newer formats before interpreting anything else: their
        // fields may mean something this engine would get wrong.
        if let Some(found) = raw.get("format").and_then(|f| f.as_u64()) {
            if found > u64::from(FORMAT) {
                return Err(Error::UnsupportedFormat {
                    found,
                    supported: FORMAT,
                });
            }
        }
        let show: Show = serde_json::from_value(raw.clone()).map_err(invalid)?;
        if show.format == 0 {
            return Err(Error::InvalidShow("format 0 does not exist".into()));
        }
        parse_color(&show.background)
            .ok_or_else(|| Error::InvalidColor(show.background.clone()))?;
        for output in std::iter::once(&show.output)
            .chain(show.scenes.iter().filter_map(|s| s.output.as_ref()))
        {
            OutputColor::from_output(output)
                .ok_or_else(|| Error::InvalidColor(output.tint.clone().unwrap_or_default()))?;
            for pass in output.passes.iter().flatten() {
                let Pass::Dots(dots) = pass;
                if !(dots.size > 0.0 && dots.size <= 1.0) {
                    return Err(Error::InvalidShow(
                        "a dots pass needs a size above 0, up to 1".into(),
                    ));
                }
                if !(0.0..=1.0).contains(&dots.glow) {
                    return Err(Error::InvalidShow(
                        "a dots pass needs a glow from 0 to 1".into(),
                    ));
                }
                if let Some(unlit) = &dots.unlit {
                    parse_color(unlit).ok_or_else(|| Error::InvalidColor(unlit.clone()))?;
                }
            }
        }
        validate(&show)?;
        for (name, style) in &show.fonts {
            let problem = if self.outline_fonts.contains_key(&style.file) {
                match style.size {
                    Some(size) if size > 0.0 => None,
                    Some(_) => Some("needs a size above 0"),
                    None => Some("uses an outline font and needs a size"),
                }
            } else if self.fonts.contains_key(&style.file) && style.size.is_some() {
                Some("uses a bitmap font, which has one fixed size: remove size")
            } else {
                None
            };
            if let Some(problem) = problem {
                return Err(Error::InvalidShow(format!("font style {name:?} {problem}")));
            }
        }
        *self.text_cache.get_mut().unwrap_or_else(|e| e.into_inner()) = TextCache::default();
        self.load_warnings.clear();
        if let Ok(understood) = serde_json::to_value(&show) {
            ignored_fields(&raw, &understood, "", &mut self.load_warnings);
        }
        self.variables = show.variables.clone();
        self.playing.clear();
        self.sounding.clear();
        self.transitions.clear();
        self.debounced.clear();
        self.reels.clear();
        (self.transition_sites, self.debounce_sites) = binding_sites(&show);
        self.reel_sites = reel_sites(&show);
        self.events.clear();
        self.time = 0.0;
        self.active_scene = (!show.scenes.is_empty()).then_some(0);
        self.show = Some(show);
        self.start_matching(Some(Root::Show), |tl| tl.autoplay);
        self.play_autoplay(Root::Show);
        if let Some(scene) = self.active_scene {
            self.start_matching(Some(Root::Scene(scene)), |tl| tl.autoplay);
            self.play_autoplay(Root::Scene(scene));
        }
        Ok(())
    }

    /// Fields of the loaded show document that the engine did not
    /// understand and ignored, as JSON paths (`layers[2].colour`): usually
    /// typos. Keys starting with `$` (like `$schema`) are never reported.
    pub fn load_warnings(&self) -> &[String] {
        &self.load_warnings
    }

    /// Push a named value from the host. Unknown names are accepted:
    /// content may bind to them later.
    pub fn set_variable(&mut self, name: &str, value: impl Into<Value>) {
        self.variables.insert(name.to_owned(), value.into());
    }

    pub fn variable(&self, name: &str) -> Option<&Value> {
        self.variables.get(name)
    }

    /// Register (or replace) a named RGBA8 image that image layers can
    /// reference. Images are host assets, not show content: they survive
    /// `load_show` and may be provided before or after the show that
    /// uses them (layers referencing a missing image are skipped).
    pub fn set_image(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        pixels: impl Into<Arc<[u8]>>,
    ) -> Result<(), Error> {
        let pixels = pixels.into();
        let expected = width as usize * height as usize * 4;
        if pixels.len() != expected {
            return Err(Error::InvalidImage(format!(
                "{name:?}: {} bytes for {width}x{height}, expected {expected}",
                pixels.len()
            )));
        }
        self.images.insert(
            name.to_owned(),
            ImageData {
                width,
                height,
                pixels,
                revision: next_revision(),
            },
        );
        Ok(())
    }

    /// Register (or replace) a named bitmap font that font styles reference
    /// by `file`: a parsed description plus its page images in page order
    /// (see [`BitmapFont::pages`]), each `(width, height, RGBA8 pixels)`.
    /// Like images, fonts are host assets that survive `load_show`.
    pub fn set_font(
        &mut self,
        name: &str,
        font: BitmapFont,
        pages: Vec<(u32, u32, Vec<u8>)>,
    ) -> Result<(), Error> {
        if pages.len() != font.pages().len() {
            return Err(Error::InvalidFont(format!(
                "{name:?}: {} page images for {} pages",
                pages.len(),
                font.pages().len()
            )));
        }
        let pages = pages
            .into_iter()
            .map(|(width, height, pixels)| {
                if pixels.len() != width as usize * height as usize * 4 {
                    return Err(Error::InvalidFont(format!(
                        "{name:?}: page of {} bytes for {width}x{height}",
                        pixels.len()
                    )));
                }
                Ok(Rgba {
                    width,
                    height,
                    pixels,
                })
            })
            .collect::<Result<_, _>>()?;
        self.outline_fonts.remove(name);
        self.fonts
            .insert(name.to_owned(), RegisteredFont { font, pages });
        *self.text_cache.get_mut().unwrap_or_else(|e| e.into_inner()) = TextCache::default();
        Ok(())
    }

    /// Register (or replace) a named outline font (TrueType / OpenType
    /// file bytes) that font styles reference by `file`, with the em size
    /// they set. Like images, fonts are host assets that survive
    /// `load_show`. Registering a name replaces a bitmap font of that name.
    #[cfg(feature = "outline-fonts")]
    pub fn set_outline_font(
        &mut self,
        name: &str,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Result<(), Error> {
        let data = bytes.into();
        crate::outline::validate(&data)
            .map_err(|e| Error::InvalidFont(format!("{name:?}: {e}")))?;
        self.fonts.remove(name);
        self.outline_fonts.insert(
            name.to_owned(),
            FontData {
                data,
                revision: next_revision(),
            },
        );
        *self.text_cache.get_mut().unwrap_or_else(|e| e.into_inner()) = TextCache::default();
        Ok(())
    }

    /// Whether a font, bitmap or outline, is registered under `name`.
    pub fn has_font(&self, name: &str) -> bool {
        self.fonts.contains_key(name) || self.outline_fonts.contains_key(name)
    }

    /// Register (or replace) a named sound by its `duration` in seconds,
    /// which is all the engine needs of it: to loop, repeat and end plays.
    /// The samples stay with the host's audio backend, which plays what
    /// [`voices`](Engine::voices) reports. Like images, sounds are host
    /// assets that survive `load_show` and may arrive after it: an audio
    /// layer whose sound is not registered plays silently and does not end
    /// until it is.
    pub fn set_sound(&mut self, name: &str, duration: f64) -> Result<(), Error> {
        if !(duration.is_finite() && duration > 0.0) {
            return Err(Error::InvalidSound(format!(
                "{name:?}: duration {duration} is not above 0"
            )));
        }
        self.sounds.insert(name.to_owned(), duration);
        Ok(())
    }

    /// The duration registered for sound `name`, in seconds.
    pub fn sound_duration(&self, name: &str) -> Option<f64> {
        self.sounds.get(name).copied()
    }

    /// The pixels registered under `name`, if any.
    pub fn image(&self, name: &str) -> Option<&ImageData> {
        self.images.get(name)
    }

    /// Register (or replace) named vector artwork that vector layers
    /// reference: paths with fills and strokes in the artwork's own units,
    /// what the loader makes of an SVG file. Like images, vectors are host
    /// assets that survive `load_show`; a layer whose vector is not (yet)
    /// registered is skipped.
    pub fn set_vector(&mut self, name: &str, vector: Vector) -> Result<(), Error> {
        if !(vector.width > 0.0 && vector.height > 0.0) {
            return Err(Error::InvalidShow(format!(
                "vector {name:?}: size {}x{} is not above 0",
                vector.width, vector.height
            )));
        }
        self.vectors.insert(name.to_owned(), vector);
        Ok(())
    }

    /// The vector artwork registered under `name`, if any.
    pub fn vector(&self, name: &str) -> Option<&Vector> {
        self.vectors.get(name)
    }

    /// Fire a named event. A scene declaring it as its trigger becomes the
    /// active scene (restarting it when already active); then every
    /// timeline declaring it, in the show's layers or the active scene,
    /// (re)starts from 0, and every audio layer declaring it plays (or
    /// stops, when it is the layer's `stop`).
    pub fn trigger(&mut self, name: &str) {
        let entered = self
            .show
            .as_ref()
            .and_then(|show| show.scenes.iter().position(|s| s.trigger.contains(name)));
        if let Some(scene) = entered {
            self.enter_scene(scene);
        }
        self.start_matching(None, |tl| tl.trigger.contains(name));
        let roots: Vec<Root> = std::iter::once(Root::Show)
            .chain(self.active_scene.map(Root::Scene))
            .collect();
        for root in roots {
            for (path, (stops, plays)) in self.audio_layers(root, |trigger, stop| {
                (stop.contains(name), trigger.contains(name))
            }) {
                if stops {
                    self.sounding
                        .retain(|s| !(s.root == root && s.layer_path == path));
                }
                if plays {
                    self.play(root, path);
                }
            }
        }
    }

    /// The audio layers of `root` for which `want` (given their `trigger`
    /// and `stop`) says something: their paths, with what it said.
    fn audio_layers<T>(
        &self,
        root: Root,
        want: impl Fn(&crate::model::Triggers, &crate::model::Triggers) -> T,
    ) -> Vec<(Vec<usize>, T)> {
        fn walk<T>(
            layers: &[Layer],
            path: &mut Vec<usize>,
            want: &impl Fn(&crate::model::Triggers, &crate::model::Triggers) -> T,
            out: &mut Vec<(Vec<usize>, T)>,
        ) {
            for (i, layer) in layers.iter().enumerate() {
                path.push(i);
                if let LayerKind::Audio { trigger, stop, .. } = &layer.kind {
                    out.push((path.clone(), want(trigger, stop)));
                }
                walk(layer.children(), path, want, out);
                path.pop();
            }
        }
        let mut out = Vec::new();
        if let Some(layers) = self.show.as_ref().and_then(|show| root_layers(show, root)) {
            walk(layers, &mut Vec::new(), &want, &mut out);
        }
        out
    }

    /// Start the autoplay audio layers of `root`.
    fn play_autoplay(&mut self, root: Root) {
        let Some(layers) = self.show.as_ref().and_then(|show| root_layers(show, root)) else {
            return;
        };
        let mut starts = Vec::new();
        fn walk(layers: &[Layer], path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            for (i, layer) in layers.iter().enumerate() {
                path.push(i);
                if let LayerKind::Audio { autoplay: true, .. } = &layer.kind {
                    out.push(path.clone());
                }
                walk(layer.children(), path, out);
                path.pop();
            }
        }
        walk(layers, &mut Vec::new(), &mut starts);
        for path in starts {
            self.play(root, path);
        }
    }

    /// Play the audio layer at `path`, as its `retrigger` says when it
    /// already plays.
    fn play(&mut self, root: Root, path: Vec<usize>) {
        let layer = self
            .show
            .as_ref()
            .and_then(|show| root_layers(show, root))
            .and_then(|layers| layer_at(layers, &path));
        let Some(LayerKind::Audio {
            retrigger, voices, ..
        }) = layer.map(|l| &l.kind)
        else {
            return;
        };
        let (retrigger, voices) = (*retrigger, *voices as usize);
        let mine = |s: &Sounding| s.root == root && s.layer_path == path;
        match retrigger {
            Retrigger::Restart => self.sounding.retain(|s| !mine(s)),
            Retrigger::Ignore if self.sounding.iter().any(mine) => return,
            Retrigger::Ignore => {}
            Retrigger::Overlap => {
                // Plays are in start order: the oldest of this layer's
                // stands first.
                let mut over = (self.sounding.iter().filter(|s| mine(s)).count() + 1)
                    .saturating_sub(voices.max(1));
                self.sounding.retain(|s| {
                    if over > 0 && mine(s) {
                        over -= 1;
                        return false;
                    }
                    true
                });
            }
        }
        self.next_voice += 1;
        self.sounding.push(Sounding {
            root,
            layer_path: path,
            id: self.next_voice,
            started: self.time,
        });
    }

    /// Name of the active scene, if the show has scenes.
    pub fn active_scene(&self) -> Option<&str> {
        let show = self.show.as_ref()?;
        Some(show.scenes.get(self.active_scene?)?.name.as_str())
    }

    /// The output in effect: what the active scene sets, over the show's.
    fn effective_output(&self) -> Output {
        let Some(show) = &self.show else {
            return Output::default();
        };
        let scene_output = self
            .active_scene
            .and_then(|i| show.scenes.get(i))
            .and_then(|s| s.output.as_ref());
        match scene_output {
            Some(output) => output.over(&show.output),
            None => show.output.clone(),
        }
    }

    /// Output color handling for the current frame (the active scene's
    /// settings over the show's). Full color when no show is loaded.
    /// Renderers apply it to the finished frame.
    pub fn output(&self) -> OutputColor {
        // Tints were validated at load.
        OutputColor::from_output(&self.effective_output()).unwrap_or_default()
    }

    /// Effects hosts apply to the current frame as they show it (the active
    /// scene's list, else the show's), in order. The presenter of the
    /// `render` feature does.
    pub fn passes(&self) -> Vec<Pass> {
        self.effective_output().passes.unwrap_or_default()
    }

    /// How hosts should scale the current frame up to their surface (the
    /// active scene's setting over the show's); see [`render::fit`].
    ///
    /// [`render::fit`]: crate::render::fit
    pub fn scaling(&self) -> Scaling {
        self.effective_output().scaling.unwrap_or_default()
    }

    fn enter_scene(&mut self, scene: usize) {
        self.playing.retain(|p| p.root == Root::Show);
        // Leaving a scene stops its sounds.
        self.sounding.retain(|s| s.root == Root::Show);
        // A scene's properties start at their values, like at load.
        self.transitions.retain(|(root, ..), _| *root == Root::Show);
        self.debounced.retain(|(root, ..), _| *root == Root::Show);
        self.reels.retain(|(root, ..), _| *root == Root::Show);
        self.active_scene = Some(scene);
        self.start_matching(Some(Root::Scene(scene)), |tl| tl.autoplay);
        self.play_autoplay(Root::Scene(scene));
    }

    /// Advance time by `dt` seconds: running timelines and sounds
    /// progress, looping ones wrap, finished ones stop (their properties
    /// fall back to bindings/base values) and fire their `on_end` trigger,
    /// which is also reported through [`drain_events`](Engine::drain_events).
    pub fn advance_frame(&mut self, dt: f64) {
        self.settle_debounces(dt);
        self.follow_transitions();
        self.follow_reels();
        self.time += dt;
        let Some(show) = &self.show else { return };
        let mut finished: Vec<usize> = Vec::new();
        let mut on_end: Vec<String> = Vec::new();
        for (i, p) in self.playing.iter_mut().enumerate() {
            p.time += dt;
            let layer = root_layers(show, p.root).and_then(|l| layer_at(l, &p.layer_path));
            let Some(tl) = layer.and_then(|l| l.timelines.get(p.timeline)) else {
                finished.push(i);
                continue;
            };
            if p.time < 0.0 {
                continue;
            }
            let duration = tl.duration();
            if tl.looping && duration > 0.0 {
                p.time %= duration;
            } else if duration <= 0.0 || p.time >= tl.play_time() {
                finished.push(i);
                on_end.extend(tl.on_end.clone());
            }
        }
        for i in finished.into_iter().rev() {
            self.playing.remove(i);
        }
        // A play ends when its time is up; a play of an unregistered sound
        // has no end yet.
        let time = self.time;
        let mut ended: Vec<usize> = Vec::new();
        for (i, s) in self.sounding.iter().enumerate() {
            let layer = root_layers(show, s.root).and_then(|l| layer_at(l, &s.layer_path));
            let Some(LayerKind::Audio {
                sound,
                looping,
                delay,
                repeat,
                on_end: end,
                ..
            }) = layer.map(|l| &l.kind)
            else {
                ended.push(i);
                continue;
            };
            if *looping {
                continue;
            }
            let Some(duration) = self.sounds.get(sound) else {
                continue;
            };
            if time - s.started - delay.max(0.0) >= duration * repeat.unwrap_or(1.0).max(0.0) {
                ended.push(i);
                on_end.extend(end.clone());
            }
        }
        for i in ended.into_iter().rev() {
            self.sounding.remove(i);
        }
        for name in on_end {
            self.trigger(&name);
            if self.events.len() == MAX_PENDING_EVENTS {
                self.events.pop_front();
            }
            self.events.push_back(Event::Trigger(name));
        }
    }

    /// Take the events raised since the last call, oldest first. This is
    /// how content talks back to the host: a show can end a sequence with
    /// `on_end` and the host reacts (award points, switch hardware, ...).
    pub fn drain_events(&mut self) -> Vec<Event> {
        self.events.drain(..).collect()
    }

    /// Seconds advanced since the show loaded.
    pub fn time(&self) -> f64 {
        self.time
    }

    pub fn show(&self) -> Option<&Show> {
        self.show.as_ref()
    }

    /// Resolve the show into a flat draw list: visible shape layers in
    /// paint order with absolute position and effective opacity. Clipped
    /// groups bracket their children with [`ResolvedShape::ClipBegin`] and
    /// [`ResolvedShape::ClipEnd`].
    ///
    /// Property precedence, strongest first: running timeline, binding,
    /// base value from the show description.
    pub fn resolved_layers(&self) -> Result<Vec<ResolvedLayer>, Error> {
        let show = self.show.as_ref().ok_or(Error::NoShow)?;
        let mut out = Vec::new();
        self.walk(
            Root::Show,
            &show.layers,
            &mut Vec::new(),
            Transform::IDENTITY,
            1.0,
            &mut out,
        )?;
        if let Some(scene) = self.active_scene {
            if let Some(layers) = root_layers(show, Root::Scene(scene)) {
                self.walk(
                    Root::Scene(scene),
                    layers,
                    &mut Vec::new(),
                    Transform::IDENTITY,
                    1.0,
                    &mut out,
                )?;
            }
        }
        Ok(out)
    }

    /// The sounds that should be heard now, in tree order: every play of a
    /// visible audio layer whose sound is registered and whose delay is
    /// over, at its position and effective gain. The audio twin of
    /// [`resolved_layers`](Engine::resolved_layers): a backend diffs it
    /// frame by frame (start what is new, stop what is gone, ramp gains,
    /// resync a position that jumped) and hosts that mix themselves read
    /// the same list.
    pub fn voices(&self) -> Result<Vec<Voice>, Error> {
        let show = self.show.as_ref().ok_or(Error::NoShow)?;
        let mut out = Vec::new();
        self.hear(Root::Show, &show.layers, &mut Vec::new(), 1.0, &mut out);
        if let Some(scene) = self.active_scene {
            if let Some(layers) = root_layers(show, Root::Scene(scene)) {
                self.hear(Root::Scene(scene), layers, &mut Vec::new(), 1.0, &mut out);
            }
        }
        Ok(out)
    }

    fn hear(
        &self,
        root: Root,
        layers: &[Layer],
        path: &mut Vec<usize>,
        chain: f64,
        out: &mut Vec<Voice>,
    ) {
        for (i, layer) in layers.iter().enumerate() {
            path.push(i);
            if self.is_visible(root, layer, path) {
                match &layer.kind {
                    LayerKind::Group { children, .. } => {
                        let gain = chain * self.number(root, layer, path, Property::Gain).max(0.0);
                        self.hear(root, children, path, gain, out);
                    }
                    LayerKind::Audio {
                        sound,
                        looping,
                        delay,
                        repeat,
                        bus,
                        ..
                    } => {
                        let gain = chain * self.number(root, layer, path, Property::Gain).max(0.0);
                        let plays = self
                            .sounding
                            .iter()
                            .filter(|s| s.root == root && s.layer_path == *path);
                        for play in plays {
                            let Some(duration) = self.sounds.get(sound) else {
                                continue;
                            };
                            let elapsed = self.time - play.started - delay.max(0.0);
                            if elapsed < 0.0 {
                                continue;
                            }
                            let position = if *looping || repeat.is_some() {
                                elapsed % duration
                            } else {
                                elapsed
                            };
                            out.push(Voice {
                                id: play.id,
                                layer: layer.name.clone(),
                                sound: sound.clone(),
                                position,
                                gain,
                                looping: *looping,
                                bus: bus.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            path.pop();
        }
    }

    /// (Re)start the timelines `want` selects, in `root` or, with `None`,
    /// in the show's layers and the active scene.
    fn start_matching(&mut self, root: Option<Root>, want: impl Fn(&Timeline) -> bool) {
        let Some(show) = &self.show else { return };
        let roots = match root {
            Some(root) => vec![root],
            None => std::iter::once(Root::Show)
                .chain(self.active_scene.map(Root::Scene))
                .collect(),
        };
        let mut starts = Vec::new();
        for root in roots {
            let Some(layers) = root_layers(show, root) else {
                continue;
            };
            collect_timelines(layers, &mut Vec::new(), &mut |path, idx, tl| {
                if want(tl) {
                    starts.push((root, path.to_vec(), idx));
                }
            });
        }
        for (root, layer_path, timeline) in starts {
            self.playing.retain(|p| {
                !(p.root == root && p.layer_path == layer_path && p.timeline == timeline)
            });
            let delay = root_layers(show, root)
                .and_then(|layers| layer_at(layers, &layer_path))
                .and_then(|l| l.timelines.get(timeline))
                .map_or(0.0, |tl| tl.delay.max(0.0));
            self.playing.push(Playhead {
                root,
                layer_path,
                timeline,
                time: -delay,
            });
        }
    }

    /// Let every binding with a debounce take in its variable: a new value
    /// becomes the candidate, and a candidate that will have held for the
    /// debounce time by the end of this step of `dt` settles, so it shows
    /// in the frame the hold runs out. A first look settles at once.
    fn settle_debounces(&mut self, dt: f64) {
        let Some(show) = &self.show else { return };
        let mut debounced = std::mem::take(&mut self.debounced);
        for site in &self.debounce_sites {
            let (root, path, index) = site;
            if *root != Root::Show && Some(*root) != self.active_scene.map(Root::Scene) {
                continue;
            }
            let binding = root_layers(show, *root)
                .and_then(|layers| layer_at(layers, path))
                .and_then(|layer| layer.bindings.get(*index));
            let Some((binding, hold)) = binding.and_then(|b| Some((b, b.debounce?))) else {
                continue;
            };
            let Some(value) = self.variables.get(&binding.variable) else {
                debounced.remove(site);
                continue;
            };
            let settling = debounced.entry(site.clone()).or_insert_with(|| Settling {
                settled: value.clone(),
                candidate: value.clone(),
                since: self.time,
            });
            if settling.candidate != *value {
                settling.candidate = value.clone();
                settling.since = self.time;
            }
            // Tolerant of a hold that ends a rounding error short.
            if settling.settled != settling.candidate
                && self.time + dt - settling.since >= hold - 1e-9
            {
                settling.settled = settling.candidate.clone();
            }
        }
        self.debounced = debounced;
    }

    /// Note which character each reel cell is heading for, as of now. A
    /// cell that is already there is left alone, so a change moves only
    /// the cells it reaches, each from wherever it stands.
    fn follow_reels(&mut self) {
        let Some(show) = &self.show else { return };
        let mut reels = std::mem::take(&mut self.reels);
        for site in &self.reel_sites {
            let (root, path) = site;
            if *root != Root::Show && Some(*root) != self.active_scene.map(Root::Scene) {
                continue;
            }
            let layer = root_layers(show, *root).and_then(|layers| layer_at(layers, path));
            let Some(layer) = layer else { continue };
            let LayerKind::Digits {
                digits,
                justify,
                display: DigitDisplay::Reel(reel),
                ..
            } = &layer.kind
            else {
                continue;
            };
            let count = *digits as usize;
            let ring = reel.characters();
            let roll = reel.roll();
            let text = self.text(*root, layer, path, Property::Text);
            let wanted: Vec<Option<f64>> = cells(&text, count, *justify)
                .into_iter()
                .map(|c| {
                    let c = c?;
                    ring.iter().position(|on| *on == c).map(|i| i as f64)
                })
                .collect();
            let records = reels.entry(site.clone()).or_insert_with(|| {
                // At load a row stands at what it shows: nothing rolls in.
                wanted
                    .iter()
                    .map(|target| {
                        let target = target.unwrap_or(0.0);
                        Change {
                            start: target,
                            target,
                            started: self.time,
                        }
                    })
                    .collect()
            });
            records.resize(
                count,
                Change {
                    start: 0.0,
                    target: 0.0,
                    started: self.time,
                },
            );
            let ring_length = reel.ring();
            for (i, character) in wanted.into_iter().enumerate() {
                let Some(character) = character else { continue };
                let change = &mut records[i];
                // Where it is heading, as a place on the ring: a cell that
                // is already going there carries on.
                if change.target.rem_euclid(ring_length) == character {
                    continue;
                }
                let reached =
                    roll.value_at(change.start, change.target, self.time - change.started);
                // The cell on the right moves first; the rest follow.
                let delay = (count - 1 - i) as f64 * reel.stagger.max(0.0);
                *change = Change {
                    start: reached,
                    target: reel.travel(reached, character),
                    started: self.time + delay,
                };
            }
        }
        self.reels = reels;
    }

    /// Where the cells of the reel row at `path` stand on their ring now.
    fn reel_positions(&self, root: Root, path: &[usize], reel: &crate::model::Reel) -> Vec<f64> {
        let roll = reel.roll();
        self.reels
            .get(&(root, path.to_vec()))
            .map(|records| {
                records
                    .iter()
                    .map(|c| roll.value_at(c.start, c.target, self.time - c.started))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Note where every binding with a transition is heading, as of now
    /// (inputs arrive between frames): a first look starts the property at
    /// its value, a new target starts a change from the value reached.
    fn follow_transitions(&mut self) {
        let Some(show) = &self.show else { return };
        let mut transitions = std::mem::take(&mut self.transitions);
        for site in &self.transition_sites {
            let (root, path, index) = site;
            if *root != Root::Show && Some(*root) != self.active_scene.map(Root::Scene) {
                continue;
            }
            let binding = root_layers(show, *root)
                .and_then(|layers| layer_at(layers, path))
                .and_then(|layer| layer.bindings.get(*index));
            let Some((binding, transition)) =
                binding.and_then(|b| Some((b, b.transition.as_ref()?)))
            else {
                continue;
            };
            let Some(target) = self.binding_number(site, binding) else {
                transitions.remove(site);
                continue;
            };
            let change = transitions.entry(site.clone()).or_insert(Change {
                start: target,
                target,
                started: self.time,
            });
            if change.target != target {
                let reached =
                    transition.value_at(change.start, change.target, self.time - change.started);
                *change = Change {
                    start: reached,
                    target,
                    started: self.time,
                };
            }
        }
        self.transitions = transitions;
    }

    /// What binding `index` of the layer at `path` holds while its
    /// transition is under way; `None` without one.
    fn in_transition(
        &self,
        root: Root,
        path: &[usize],
        index: usize,
        b: &Binding,
    ) -> Option<Value> {
        let transition = b.transition.as_ref()?;
        let change = self.transitions.get(&(root, path.to_vec(), index))?;
        let mut n = transition.value_at(change.start, change.target, self.time - change.started);
        if b.property != Property::Text {
            return Some(Value::Number(n));
        }
        // A counter between whole numbers shows whole numbers.
        if change.start.fract() == 0.0 && change.target.fract() == 0.0 {
            n = n.round();
        }
        Some(Value::Text(b.format.format(n)))
    }

    /// Resolve a layer property: its base value, overridden by bindings
    /// (eased by their transitions), overridden by a running timeline
    /// (numeric properties only). `None` when this kind of layer does not
    /// have the property.
    fn resolve(&self, root: Root, layer: &Layer, path: &[usize], prop: Property) -> Option<Value> {
        let mut v = layer.base_value(prop)?;
        let bindings = layer.bindings.iter().enumerate();
        for (index, b) in bindings.filter(|(_, b)| b.property == prop) {
            let bound = self.in_transition(root, path, index, b).or_else(|| {
                self.binding_value(&(root, path.to_vec(), index), b)
                    .and_then(|value| self.convert(b, value))
            });
            if let Some(bound) = bound {
                v = bound;
            }
        }
        if !prop.is_numeric() {
            return Some(v);
        }
        // A running timeline owns the property.
        for p in &self.playing {
            if p.root != root || p.layer_path != path {
                continue;
            }
            let Some(tl) = layer.timelines.get(p.timeline) else {
                continue;
            };
            // Still waiting out its delay: it does not own anything yet.
            let Some(time) = tl.local_time(p.time) else {
                continue;
            };
            for track in tl.tracks.iter().filter(|t| t.property == prop) {
                if let Some(sampled) = track.sample(time) {
                    v = Value::Number(sampled);
                }
            }
        }
        Some(v)
    }

    fn number(&self, root: Root, layer: &Layer, path: &[usize], prop: Property) -> f64 {
        self.resolve(root, layer, path, prop)
            .map_or(0.0, |v| v.as_number())
    }

    fn text(&self, root: Root, layer: &Layer, path: &[usize], prop: Property) -> String {
        self.resolve(root, layer, path, prop)
            .map_or_else(String::new, |v| v.to_text())
    }

    /// The value a binding at `site` feeds its property: the variable's
    /// (as debounced), or what `map`/`default` turn it into. `None` when
    /// it does not apply.
    fn binding_value(&self, site: &TransitionSite, b: &Binding) -> Option<Value> {
        let value = match (b.debounce, self.debounced.get(site)) {
            (Some(_), Some(settling)) => &settling.settled,
            _ => self.variables.get(&b.variable)?,
        };
        match &b.map {
            None => Some(value.clone()),
            Some(map) => map.get(&value.to_text()).or(b.default.as_ref()).cloned(),
        }
    }

    /// The number a binding's transition eases toward: its value after
    /// `map`, `threshold`, `scale` and `offset`. `None` when that is not
    /// a number.
    fn binding_number(&self, site: &TransitionSite, b: &Binding) -> Option<f64> {
        let value = self.binding_value(site, b)?;
        let n = match (b.property, &value) {
            (Property::Font, _) => return None,
            (Property::Text, Value::Number(n)) => *n,
            (Property::Text, _) => return None,
            _ => value.as_number(),
        };
        Some(scaled(b, n))
    }

    /// Turn a bound value into what the binding's property holds. `None`
    /// leaves the property as it was.
    fn convert(&self, b: &Binding, value: Value) -> Option<Value> {
        match b.property {
            Property::Text => Some(Value::Text(match value {
                Value::Number(n) => b.format.format(scaled(b, n)),
                other => other.to_text(),
            })),
            Property::Visible => Some(Value::Bool(scaled(b, value.as_number()) != 0.0)),
            // Only declared font styles apply.
            Property::Font => match value {
                Value::Text(style) if self.show.as_ref()?.fonts.contains_key(&style) => {
                    Some(Value::Text(style))
                }
                _ => None,
            },
            _ => Some(Value::Number(scaled(b, value.as_number()))),
        }
    }

    /// Whether the layer at `path` shows and sounds: its `visible`, as
    /// bound.
    fn is_visible(&self, root: Root, layer: &Layer, path: &[usize]) -> bool {
        match self.resolve(root, layer, path, Property::Visible) {
            Some(Value::Bool(on)) => on,
            Some(Value::Number(n)) => n != 0.0,
            Some(Value::Text(t)) => !t.is_empty(),
            None => layer.visible,
        }
    }

    /// A layer's content box `[x, y, width, height]` in its local space,
    /// before any scale; `None` for groups, unregistered images and text
    /// whose font is not registered.
    fn content_box(&self, root: Root, layer: &Layer, path: &[usize]) -> Option<[f64; 4]> {
        let [x, y, w, h] = match &layer.kind {
            LayerKind::Group { .. } | LayerKind::Audio { .. } => return None,
            LayerKind::Shape { shape, .. } => match shape {
                Shape::Rect(rect) => *rect,
                Shape::Circle([cx, cy, r]) => [cx - r, cy - r, 2.0 * r, 2.0 * r],
                Shape::Path(data) => path::bounds(data.elements())?,
            },
            LayerKind::Vector { vector, size } => {
                let data = self.vectors.get(vector)?;
                let [w, h] = size.unwrap_or([data.width, data.height]);
                [0.0, 0.0, w, h]
            }
            LayerKind::Image {
                image, size, sheet, ..
            } => {
                let data = self.images.get(image)?;
                let natural = match sheet {
                    Some(sheet) => sheet.cell.map(f64::from),
                    None => [f64::from(data.width), f64::from(data.height)],
                };
                let [w, h] = size.unwrap_or(natural);
                [0.0, 0.0, w, h]
            }
            LayerKind::Digits { size: [w, h], .. } => [0.0, 0.0, *w, *h],
            // The text's box: its size, or the measured text.
            LayerKind::Text { size, align, .. } => {
                let [w, h] = match size {
                    Some(size) => *size,
                    None => {
                        let text = self.text(root, layer, path, Property::Text);
                        let font = self.text(root, layer, path, Property::Font);
                        match self.text_draw(&font, &text, None, *align)? {
                            TextDraw::Bitmap(raster) => raster.container,
                            TextDraw::Glyphs { container, .. } => container,
                        }
                    }
                };
                [0.0, 0.0, w, h]
            }
        };
        Some([x, y, w, h])
    }

    /// How `text` in font style `style_name` gets drawn: a glyph run for an
    /// outline font, a raster for a bitmap font. `None` when the style's
    /// font is not registered (or is an outline font without a `size`).
    fn text_draw(
        &self,
        style_name: &str,
        text: &str,
        size: Option<[f64; 2]>,
        align: Align,
    ) -> Option<TextDraw> {
        #[cfg(feature = "outline-fonts")]
        if let Some((style, font)) = self
            .show
            .as_ref()
            .and_then(|show| show.fonts.get(style_name))
            .and_then(|style| Some((style, self.outline_fonts.get(&style.file)?)))
        {
            let layout = crate::outline::layout(font, text, style.size?, size, align)?;
            return Some(TextDraw::Glyphs {
                font: font.clone(),
                size: style.size?,
                glyphs: layout.glyphs,
                container: layout.container,
            });
        }
        self.text_raster(style_name, text, size, align)
            .map(TextDraw::Bitmap)
    }

    /// Rasterize (or fetch from cache) `text` in font style `style`.
    /// `None` when the style's font is not registered or nothing draws.
    fn text_raster(
        &self,
        style_name: &str,
        text: &str,
        size: Option<[f64; 2]>,
        align: Align,
    ) -> Option<Arc<TextRaster>> {
        let style = self.show.as_ref()?.fonts.get(style_name)?;
        let registered = self.fonts.get(&style.file)?;
        let mut cache = self.text_cache.lock().unwrap_or_else(|e| e.into_inner());
        let key = format!("{style_name}\u{1}{text}\u{1}{size:?}\u{1}{align:?}");
        if let Some(raster) = cache.rasters.get(&key) {
            return raster.clone();
        }
        let styled = cache
            .styled
            .entry(style_name.to_owned())
            .or_insert_with(|| {
                // Colors were validated at load.
                let rgb = |c: &str| {
                    let [r, g, b, _] = parse_color(c).unwrap_or([255; 4]);
                    [r, g, b]
                };
                let border = style.border.as_ref().map(|b| (rgb(&b.color), b.width));
                Arc::new(StyledFont::new(
                    &registered.font,
                    &registered.pages,
                    rgb(&style.color),
                    border,
                ))
            })
            .clone();
        let raster = styled
            .rasterize(text, size, align)
            .map(|(rgba, offset, container)| {
                Arc::new(TextRaster {
                    image: ImageData::generated(rgba),
                    offset,
                    container,
                })
            });
        let bytes = key.len() + raster.as_ref().map_or(0, |r| r.image.pixels.len());
        cache.rasters.insert(key, raster.clone(), bytes);
        raster
    }

    /// Add `text` in font style `style_name` to the draw list, laid out in
    /// a box of `size` (the text's own size when `None`) whose top-left
    /// corner sits at the item's origin. Text layers and the characters of
    /// a reel both come through here.
    fn push_text(
        &self,
        out: &mut Vec<ResolvedLayer>,
        placed: &Placed,
        style_name: &str,
        text: &str,
        size: Option<[f64; 2]>,
        align: Align,
    ) {
        let Placed {
            name,
            origin: [x, y],
            scale,
            opacity,
            blend,
            transform,
        } = *placed;
        match self.text_draw(style_name, text, size, align) {
            Some(TextDraw::Bitmap(raster)) => {
                let [ox, oy] = raster.offset;
                out.push(ResolvedLayer {
                    name: name.to_owned(),
                    shape: ResolvedShape::Bitmap {
                        x: x + f64::from(ox) * scale,
                        y: y + f64::from(oy) * scale,
                        width: f64::from(raster.image.width) * scale,
                        height: f64::from(raster.image.height) * scale,
                        image: raster.image.clone(),
                    },
                    color: [255, 255, 255, 255],
                    opacity,
                    blend,
                    transform,
                });
            }
            Some(TextDraw::Glyphs {
                font: data,
                size: em,
                glyphs,
                ..
            }) if !glyphs.is_empty() => {
                // Colors were validated at load.
                let style = self.show.as_ref().and_then(|s| s.fonts.get(style_name));
                let rgba = |c: &str| parse_color(c).unwrap_or([255; 4]);
                let border = style
                    .and_then(|s| s.border.as_ref())
                    .map(|b| (rgba(&b.color), f64::from(b.width) * scale));
                out.push(ResolvedLayer {
                    name: name.to_owned(),
                    shape: ResolvedShape::GlyphRun {
                        font: data,
                        size: em * scale,
                        glyphs: glyphs
                            .into_iter()
                            .map(|g| PlacedGlyph {
                                id: g.id,
                                x: x + g.x * scale,
                                y: y + g.y * scale,
                            })
                            .collect(),
                        border,
                    },
                    color: style.map_or([255; 4], |s| rgba(&s.color)),
                    opacity,
                    blend,
                    transform,
                });
            }
            _ => {}
        }
    }

    /// Add the artwork of ring place `index` to the draw list, fitted into
    /// a box of `size` at the item's origin and centred in it, so a symbol
    /// keeps its shape whatever shape the cells are.
    fn push_artwork(
        &self,
        out: &mut Vec<ResolvedLayer>,
        placed: &Placed,
        cells: &crate::model::ReelCells,
        index: usize,
        [box_width, box_height]: [f64; 2],
    ) {
        use crate::model::ReelCells;
        let Some(name) = cells.at(index) else { return };
        let [x, y] = placed.origin;
        // How the artwork's own size fits the box, and where that leaves it.
        let fitted = |width: f64, height: f64| {
            let fit = (box_width / width).min(box_height / height);
            (
                fit,
                (box_width - width * fit) / 2.0,
                (box_height - height * fit) / 2.0,
            )
        };
        match cells {
            ReelCells::Vectors(_) => {
                // Missing artwork is skipped, as a missing image is.
                let Some(data) = self.vectors.get(name) else {
                    return;
                };
                if data.width <= 0.0 || data.height <= 0.0 {
                    return;
                }
                let (fit, left, top) = fitted(data.width, data.height);
                let unit = fit * placed.scale;
                let origin = [x + left * placed.scale, y + top * placed.scale];
                for item in &data.paths {
                    out.push(ResolvedLayer {
                        name: placed.name.to_owned(),
                        shape: ResolvedShape::Path {
                            elements: item
                                .elements
                                .iter()
                                .map(|e| {
                                    e.map(|[px, py]| [origin[0] + px * unit, origin[1] + py * unit])
                                })
                                .collect(),
                            stroke: item.stroke.map(|(color, width)| (color, width * unit)),
                        },
                        color: item.fill.unwrap_or([0; 4]),
                        opacity: placed.opacity,
                        blend: placed.blend,
                        transform: placed.transform,
                    });
                }
            }
            ReelCells::Images(_) => {
                let Some(data) = self.images.get(name) else {
                    return;
                };
                let (natural_width, natural_height) =
                    (f64::from(data.width), f64::from(data.height));
                if natural_width <= 0.0 || natural_height <= 0.0 {
                    return;
                }
                let (fit, left, top) = fitted(natural_width, natural_height);
                out.push(ResolvedLayer {
                    name: placed.name.to_owned(),
                    shape: ResolvedShape::Image {
                        image: name.to_owned(),
                        source: None,
                        x: x + left * placed.scale,
                        y: y + top * placed.scale,
                        width: natural_width * fit * placed.scale,
                        height: natural_height * fit * placed.scale,
                    },
                    color: [255; 4],
                    opacity: placed.opacity,
                    blend: placed.blend,
                    transform: placed.transform,
                });
            }
        }
    }

    /// How far down a character has to move for the ink of `characters`
    /// to sit in the middle of the box it is laid out in, rather than the
    /// line they share. 0 when the font cannot say.
    ///
    /// A line reserves room for descenders whether the characters use any
    /// or not, so a row of digits drawn in it sits high and leaves a gap
    /// beneath. Measuring the whole ring at once, rather than each symbol
    /// as it comes, keeps the row still while it rolls: a ring that holds
    /// a descender reserves it, one of digits does not.
    fn ink_centring(&self, style_name: &str, characters: &[char]) -> f64 {
        let Some(style) = self
            .show
            .as_ref()
            .and_then(|show| show.fonts.get(style_name))
        else {
            return 0.0;
        };
        #[cfg(feature = "outline-fonts")]
        if let (Some(font), Some(size)) = (self.outline_fonts.get(&style.file), style.size) {
            if let Some((top, bottom, line)) = crate::outline::ink(font, size, characters) {
                return (line - (bottom - top)) / 2.0 - top;
            }
        }
        let Some(registered) = self.fonts.get(&style.file) else {
            return 0.0;
        };
        let cache = self.text_cache.lock().unwrap_or_else(|e| e.into_inner());
        let styled = cache.styled.get(style_name).cloned();
        drop(cache);
        let styled = styled.unwrap_or_else(|| {
            // Colors were validated at load.
            let rgb = |c: &str| {
                let [r, g, b, _] = parse_color(c).unwrap_or([255; 4]);
                [r, g, b]
            };
            let border = style.border.as_ref().map(|b| (rgb(&b.color), b.width));
            Arc::new(StyledFont::new(
                &registered.font,
                &registered.pages,
                rgb(&style.color),
                border,
            ))
        });
        match styled.ink(characters) {
            Some((top, bottom)) => (styled.line() - (bottom - top)) / 2.0 - top,
            None => 0.0,
        }
    }

    /// Add a reel row to the draw list: each cell a window on its ring,
    /// showing the character it stands on and the one coming after it,
    /// slid by how far between the two it is. A cell whose character is
    /// not on the ring shows nothing.
    #[allow(clippy::too_many_arguments)]
    fn push_reel(
        &self,
        out: &mut Vec<ResolvedLayer>,
        placed: &Placed,
        reel: &crate::model::Reel,
        text: &str,
        [width, height]: [f64; 2],
        (count, justify): (usize, Justify),
        positions: Vec<f64>,
    ) {
        let ring = reel.characters();
        if ring.is_empty() || count == 0 {
            return;
        }
        let [x, y] = placed.origin;
        let scale = placed.scale;
        // The cell's box in the layer's own units, for the font to lay a
        // character out in, and on the canvas, for placing it.
        let window = reel.window.max(1);
        let cell = [width / count as f64, height / f64::from(window)];
        let (cell_w, character_h) = (cell[0] * scale, cell[1] * scale);
        let cell_h = character_h * f64::from(window);
        // Where the character a cell stands on sits in its window.
        let middle = (f64::from(window) - 1.0) / 2.0;
        // A cell is a window its symbol should sit in the middle of, so
        // the characters are centred on their ink, as artwork is on its
        // own box, rather than on the line they are laid out in.
        let centring = match (&reel.cells, &reel.font) {
            (None, Some(font)) => self.ink_centring(font, &ring) * scale,
            _ => 0.0,
        };
        for (i, character) in cells(text, count, justify).into_iter().enumerate() {
            if character.is_none_or(|c| !ring.contains(&c)) {
                continue;
            }
            let position = positions.get(i).copied().unwrap_or_default();
            let cell_x = x + i as f64 * cell_w;
            let marker = |shape| ResolvedLayer {
                name: placed.name.to_owned(),
                shape,
                color: [0; 4],
                opacity: placed.opacity,
                blend: Blend::Normal,
                transform: placed.transform,
            };
            // Only what stands in the window shows.
            out.push(marker(ResolvedShape::ClipBegin {
                shape: Box::new(ResolvedShape::Rect {
                    x: cell_x,
                    y,
                    width: cell_w,
                    height: cell_h,
                }),
            }));
            // Every character the window can see, from the one leaving at
            // its top to the one coming up at its bottom.
            let first = (position - middle).floor();
            for k in 0..=window {
                let at = first + f64::from(k);
                let slide = at - position + middle;
                let on = at.rem_euclid(ring.len() as f64) as usize;
                let placed = Placed {
                    origin: [cell_x, y + slide * character_h + centring],
                    ..*placed
                };
                match (&reel.cells, &reel.font) {
                    (Some(cells), _) => self.push_artwork(out, &placed, cells, on, cell),
                    (None, Some(font)) => {
                        let mut buffer = [0u8; 4];
                        let character = ring[on].encode_utf8(&mut buffer);
                        self.push_text(out, &placed, font, character, Some(cell), Align::Center);
                    }
                    (None, None) => {}
                }
            }
            out.push(marker(ResolvedShape::ClipEnd));
        }
    }

    /// Resolve `layers` under `parent`, the placement of the tree above
    /// them, at `oa` opacity.
    fn walk(
        &self,
        root: Root,
        layers: &[Layer],
        path: &mut Vec<usize>,
        parent: Transform,
        oa: f64,
        out: &mut Vec<ResolvedLayer>,
    ) -> Result<(), Error> {
        for (i, layer) in layers.iter().enumerate() {
            path.push(i);
            if self.is_visible(root, layer, path) {
                let number = |prop| self.number(root, layer, path, prop);
                let opacity = (oa * number(Property::Opacity)).clamp(0.0, 1.0);
                let scale = number(Property::Scale);
                let (sx, sy) = (
                    scale * number(Property::ScaleX),
                    scale * number(Property::ScaleY),
                );
                // The anchor point, in the layer's scaled space: it lands on
                // x/y and the layer turns and scales around it.
                let pivot = layer
                    .anchor
                    .and_then(|anchor| {
                        let [bx, by, bw, bh] = self.content_box(root, layer, path)?;
                        let (ax, ay) = anchor.offset(bw, bh, 0.0, 0.0);
                        Some([(bx - ax) * sx, (by - ay) * sy])
                    })
                    .unwrap_or([0.0, 0.0]);
                let local = Transform::translate(number(Property::X), number(Property::Y))
                    .then(Transform::rotate(number(Property::Rotation)))
                    .then(Transform::translate(-pivot[0], -pivot[1]))
                    .then(Transform::scale(sx, sy));
                let m = parent.then(local);
                // Baked into the coordinates where it can be; otherwise the
                // shape stays local and the transform carries it.
                let ((scale, x, y), transform) = match m.plain() {
                    Some(plain) => (plain, Transform::IDENTITY),
                    None => ((1.0, 0.0, 0.0), m),
                };
                match &layer.kind {
                    // Heard, not seen.
                    LayerKind::Audio { .. } => {}
                    LayerKind::Group { children, clip, .. } => {
                        // A blended group is composited as one picture.
                        let mut marker = |shape: ResolvedShape| {
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape,
                                color: [0; 4],
                                opacity,
                                blend: Blend::Normal,
                                transform,
                            });
                        };
                        if layer.blend != Blend::Normal {
                            marker(ResolvedShape::BlendBegin { blend: layer.blend });
                        }
                        if let Some(clip) = clip {
                            marker(ResolvedShape::ClipBegin {
                                shape: Box::new(resolve_shape(clip, x, y, scale, None)),
                            });
                        }
                        self.walk(root, children, path, m, opacity, out)?;
                        if clip.is_some() {
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape: ResolvedShape::ClipEnd,
                                color: [0; 4],
                                opacity,
                                blend: Blend::Normal,
                                transform,
                            });
                        }
                        if layer.blend != Blend::Normal {
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape: ResolvedShape::BlendEnd,
                                color: [0; 4],
                                opacity,
                                blend: Blend::Normal,
                                transform,
                            });
                        }
                    }
                    LayerKind::Shape {
                        shape,
                        fill,
                        stroke,
                    } => {
                        let color =
                            parse_color(fill).ok_or_else(|| Error::InvalidColor(fill.clone()))?;
                        let stroke = stroke
                            .as_ref()
                            .map(|s| {
                                let color = parse_color(&s.color)
                                    .ok_or_else(|| Error::InvalidColor(s.color.clone()))?;
                                Ok::<_, Error>((color, s.width * scale))
                            })
                            .transpose()?;
                        out.push(ResolvedLayer {
                            name: layer.name.clone(),
                            shape: resolve_shape(shape, x, y, scale, stroke),
                            color,
                            opacity,
                            blend: layer.blend,
                            transform,
                        });
                    }
                    LayerKind::Vector { vector, size } => {
                        // Missing artwork is skipped like a missing image.
                        if let Some(data) = self.vectors.get(vector) {
                            let [w, h] = size.unwrap_or([data.width, data.height]);
                            let (sx, sy) = (w / data.width * scale, h / data.height * scale);
                            for item in &data.paths {
                                out.push(ResolvedLayer {
                                    name: layer.name.clone(),
                                    shape: ResolvedShape::Path {
                                        elements: item
                                            .elements
                                            .iter()
                                            .map(|e| e.map(|[px, py]| [x + px * sx, y + py * sy]))
                                            .collect(),
                                        stroke: item.stroke.map(|(c, w)| (c, w * (sx + sy) / 2.0)),
                                    },
                                    color: item.fill.unwrap_or([0; 4]),
                                    opacity,
                                    blend: layer.blend,
                                    transform,
                                });
                            }
                        }
                    }
                    LayerKind::Image {
                        image,
                        size,
                        sheet,
                        tint,
                        ..
                    } => {
                        // Missing images are skipped, not an error: the
                        // host may provide them later.
                        if let Some(data) = self.images.get(image) {
                            let source = sheet.map(|sheet| {
                                let frame = self.number(root, layer, path, Property::Frame);
                                sheet_cell(sheet, data.width, data.height, frame)
                            });
                            let natural = match source {
                                Some([_, _, w, h]) => [f64::from(w), f64::from(h)],
                                None => [f64::from(data.width), f64::from(data.height)],
                            };
                            let [width, height] = size.unwrap_or(natural);
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape: ResolvedShape::Image {
                                    image: image.clone(),
                                    source,
                                    x,
                                    y,
                                    width: width * scale,
                                    height: height * scale,
                                },
                                // Validated at load.
                                color: tint.as_deref().and_then(parse_color).unwrap_or([255; 4]),
                                opacity,
                                blend: layer.blend,
                                transform,
                            });
                        }
                    }
                    LayerKind::Digits {
                        digits,
                        size: [width, height],
                        justify,
                        display,
                        ..
                    } => {
                        let text = self.text(root, layer, path, Property::Text);
                        let placed = Placed {
                            name: &layer.name,
                            origin: [x, y],
                            scale,
                            opacity,
                            blend: layer.blend,
                            transform,
                        };
                        let cells = (*digits as usize, *justify);
                        match display {
                            DigitDisplay::Segments { style, fill, unlit } => {
                                let lit = parse_color(fill)
                                    .ok_or_else(|| Error::InvalidColor(fill.clone()))?;
                                let unlit = unlit
                                    .as_ref()
                                    .map(|c| {
                                        parse_color(c).ok_or_else(|| Error::InvalidColor(c.clone()))
                                    })
                                    .transpose()?;
                                let masks = segments::masks(*style, &text, cells.0, cells.1);
                                // Where the frame is made on the canvas's own
                                // pixel grid (anything but smooth full color,
                                // see the presenter), segments keep to it.
                                let output = self.effective_output();
                                let snap = (output.mode.unwrap_or_default()
                                    != crate::model::OutputMode::Rgb
                                    || output.scaling.unwrap_or_default() == Scaling::PixelPerfect)
                                    && transform == Transform::IDENTITY;
                                let cell_w = width * scale / cells.0.max(1) as f64;
                                for (i, mask) in masks.into_iter().enumerate() {
                                    let cell = [x + i as f64 * cell_w, y, cell_w, height * scale];
                                    let mut push = |mask: u16, color: [u8; 4]| {
                                        for points in segments::polygons(*style, mask, cell, snap) {
                                            out.push(ResolvedLayer {
                                                name: layer.name.clone(),
                                                shape: ResolvedShape::Polygon { points },
                                                color,
                                                opacity,
                                                blend: layer.blend,
                                                transform,
                                            });
                                        }
                                    };
                                    if let Some(unlit) = unlit {
                                        push(!mask, unlit);
                                    }
                                    push(mask, lit);
                                }
                            }
                            DigitDisplay::Reel(reel) => self.push_reel(
                                out,
                                &placed,
                                reel,
                                &text,
                                [*width, *height],
                                cells,
                                self.reel_positions(root, path, reel),
                            ),
                        }
                    }
                    LayerKind::Text { size, align, .. } => {
                        let text = self.text(root, layer, path, Property::Text);
                        let font = self.text(root, layer, path, Property::Font);
                        let placed = Placed {
                            name: &layer.name,
                            origin: [x, y],
                            scale,
                            opacity,
                            blend: layer.blend,
                            transform,
                        };
                        self.push_text(out, &placed, &font, &text, *size, *align);
                    }
                }
            }
            path.pop();
        }
        Ok(())
    }
}

/// Where a piece of a layer lands, shared by everything the walk adds.
#[derive(Debug, Clone, Copy)]
struct Placed<'a> {
    name: &'a str,
    /// Top-left corner on the canvas.
    origin: [f64; 2],
    scale: f64,
    opacity: f64,
    blend: Blend,
    transform: Transform,
}

/// The characters of `text` laid into `count` cells, `None` where a cell
/// has nothing to show. Text longer than the row is cut at the far side
/// of `justify`, as a digit row's text is.
fn cells(text: &str, count: usize, justify: Justify) -> Vec<Option<char>> {
    let characters: Vec<char> = text.chars().collect();
    match justify {
        Justify::Right => {
            let skip = characters.len().saturating_sub(count);
            let mut out = vec![None; count.saturating_sub(characters.len())];
            out.extend(characters.into_iter().skip(skip).map(Some));
            out
        }
        _ => {
            let mut out: Vec<Option<char>> = characters.into_iter().map(Some).collect();
            out.resize(count, None);
            out
        }
    }
}

/// Where the show's reel rows are.
fn reel_sites(show: &Show) -> Vec<(Root, Vec<usize>)> {
    fn walk(
        root: Root,
        layers: &[Layer],
        path: &mut Vec<usize>,
        out: &mut Vec<(Root, Vec<usize>)>,
    ) {
        for (i, layer) in layers.iter().enumerate() {
            path.push(i);
            if let LayerKind::Digits {
                display: DigitDisplay::Reel(_),
                ..
            } = &layer.kind
            {
                out.push((root, path.clone()));
            }
            walk(root, layer.children(), path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(Root::Show, &show.layers, &mut Vec::new(), &mut out);
    for (i, scene) in show.scenes.iter().enumerate() {
        walk(Root::Scene(i), &scene.layers, &mut Vec::new(), &mut out);
    }
    out
}

fn root_layers(show: &Show, root: Root) -> Option<&[Layer]> {
    match root {
        Root::Show => Some(&show.layers),
        Root::Scene(i) => show.scenes.get(i).map(|s| s.layers.as_slice()),
    }
}

/// What is wrong with a set of `offset` keys, if anything: they carry
/// motion added on top of a move, so they run forward from 0 and have to
/// come back to where they started.
fn offset_problem(offset: &[crate::model::Key]) -> Option<&'static str> {
    if offset.windows(2).any(|pair| pair[1].t < pair[0].t)
        || offset.iter().any(|k| k.t.is_nan() || k.t < 0.0)
    {
        return Some("needs offset keys in time order, from 0 on");
    }
    let ends = [offset.first(), offset.last()];
    if ends.iter().flatten().any(|k| k.v != 0.0) {
        return Some("needs an offset that starts and ends at 0");
    }
    None
}

/// A binding's number after `threshold`, `scale` and `offset`.
fn scaled(b: &Binding, n: f64) -> f64 {
    let n = match b.threshold {
        Some(level) => {
            if n >= level {
                1.0
            } else {
                0.0
            }
        }
        None => n,
    };
    n * b.scale + b.offset
}

/// Where the show's bindings with a transition are, and those with a
/// debounce.
fn binding_sites(show: &Show) -> (Vec<TransitionSite>, Vec<TransitionSite>) {
    type Sites = (Vec<TransitionSite>, Vec<TransitionSite>);
    fn walk(root: Root, layers: &[Layer], path: &mut Vec<usize>, out: &mut Sites) {
        for (i, layer) in layers.iter().enumerate() {
            path.push(i);
            for (index, binding) in layer.bindings.iter().enumerate() {
                if binding.transition.is_some() {
                    out.0.push((root, path.clone(), index));
                }
                if binding.debounce.is_some() {
                    out.1.push((root, path.clone(), index));
                }
            }
            walk(root, layer.children(), path, out);
            path.pop();
        }
    }
    let mut out = (Vec::new(), Vec::new());
    walk(Root::Show, &show.layers, &mut Vec::new(), &mut out);
    for (i, scene) in show.scenes.iter().enumerate() {
        walk(Root::Scene(i), &scene.layers, &mut Vec::new(), &mut out);
    }
    out
}

/// Checks `load_show` does beyond parsing: colors parse, text layers use
/// declared font styles, only numeric properties are keyframed.
fn validate(show: &Show) -> Result<(), Error> {
    for style in show.fonts.values() {
        for color in std::iter::once(&style.color).chain(style.border.as_ref().map(|b| &b.color)) {
            parse_color(color).ok_or_else(|| Error::InvalidColor(color.clone()))?;
        }
    }
    fn layers(show: &Show, list: &[Layer]) -> Result<(), Error> {
        for layer in list {
            if let LayerKind::Shape {
                stroke: Some(stroke),
                ..
            } = &layer.kind
            {
                parse_color(&stroke.color)
                    .ok_or_else(|| Error::InvalidColor(stroke.color.clone()))?;
                if !(stroke.width.is_finite() && stroke.width > 0.0) {
                    return Err(Error::InvalidShow(format!(
                        "layer {:?} needs a stroke width above 0",
                        layer.name
                    )));
                }
            }
            let font = match &layer.kind {
                LayerKind::Text { font, .. } => Some(font),
                LayerKind::Digits {
                    display: DigitDisplay::Reel(reel),
                    ..
                } => reel.font.as_ref(),
                _ => None,
            };
            if let Some(font) = font.filter(|font| !show.fonts.contains_key(*font)) {
                return Err(Error::InvalidShow(format!(
                    "layer {:?} uses undeclared font style {font:?}",
                    layer.name
                )));
            }
            if let LayerKind::Digits {
                display: DigitDisplay::Reel(reel),
                ..
            } = &layer.kind
            {
                let problem = if reel.charset.is_empty() {
                    Some("needs a charset with a character in it")
                } else if !(reel.duration.is_finite() && reel.duration > 0.0) {
                    Some("needs a duration above 0")
                } else if !reel.stagger.is_finite() || reel.stagger < 0.0 {
                    Some("needs a stagger of 0 or more")
                } else if reel.font.is_none() && reel.cells.is_none() {
                    Some("needs a font for its characters, or cells to draw instead")
                } else if reel
                    .cells
                    .as_ref()
                    .is_some_and(|cells| cells.len() != reel.charset.chars().count())
                {
                    Some("needs one cell for every character of its charset")
                } else if reel.window == 0 {
                    Some("needs a window of at least one character")
                } else if reel
                    .step
                    .is_some_and(|step| !step.is_finite() || step <= 0.0)
                {
                    Some("needs a step above 0, or none at all to travel in one move")
                } else {
                    offset_problem(&reel.offset)
                };
                if let Some(problem) = problem {
                    return Err(Error::InvalidShow(format!(
                        "reel of layer {:?} {problem}",
                        layer.name
                    )));
                }
            }
            // Properties must exist on this kind of layer; only numeric
            // ones can be keyframed.
            let tracks = layer.timelines.iter().flat_map(|tl| &tl.tracks);
            let used = tracks
                .map(|t| t.property)
                .chain(layer.bindings.iter().map(|b| b.property));
            for property in used {
                if layer.base_value(property).is_none() {
                    return Err(Error::InvalidShow(format!(
                        "layer {:?} has no {property:?} property",
                        layer.name
                    )));
                }
            }
            for timeline in &layer.timelines {
                if timeline.looping && timeline.repeat.is_some() {
                    return Err(Error::InvalidShow(format!(
                        "timeline {:?} of layer {:?} sets both loop and repeat",
                        timeline.name, layer.name
                    )));
                }
                if let Some(track) = timeline.tracks.iter().find(|t| !t.property.is_numeric()) {
                    return Err(Error::InvalidShow(format!(
                        "timeline {:?} of layer {:?} animates {:?}, which can only be bound",
                        timeline.name, layer.name, track.property
                    )));
                }
            }
            for binding in &layer.bindings {
                if binding.threshold.is_some_and(|t| !t.is_finite()) {
                    return Err(Error::InvalidShow(format!(
                        "the {:?} binding of layer {:?} needs a finite threshold",
                        binding.property, layer.name
                    )));
                }
                if binding.debounce.is_some_and(|d| !d.is_finite() || d < 0.0) {
                    return Err(Error::InvalidShow(format!(
                        "the {:?} binding of layer {:?} needs a debounce of 0 or more",
                        binding.property, layer.name
                    )));
                }
                if let Some(transition) = &binding.transition {
                    let positive = |n: f64| n.is_finite() && n > 0.0;
                    let problem = if matches!(binding.property, Property::Font | Property::Visible)
                    {
                        Some("is on a binding that cannot be eased")
                    } else if !positive(transition.duration) {
                        Some("needs a duration above 0")
                    } else if transition.wrap.is_some_and(|wrap| !positive(wrap)) {
                        Some("needs a wrap above 0")
                    } else if transition.direction.is_some() && transition.wrap.is_none() {
                        Some("sets a direction, which needs wrap")
                    } else if transition.step.is_some_and(|step| !positive(step)) {
                        Some("needs a step above 0")
                    } else {
                        offset_problem(&transition.offset)
                    };
                    if let Some(problem) = problem {
                        return Err(Error::InvalidShow(format!(
                            "transition of the {:?} binding of layer {:?} {problem}",
                            binding.property, layer.name
                        )));
                    }
                }
                if binding.property != Property::Font {
                    continue;
                }
                let mapped = binding.map.iter().flat_map(|m| m.values());
                for value in mapped.chain(&binding.default) {
                    let known =
                        matches!(value, Value::Text(style) if show.fonts.contains_key(style));
                    if !known {
                        return Err(Error::InvalidShow(format!(
                            "font binding of layer {:?} maps to {value:?}, not a declared font style",
                            layer.name
                        )));
                    }
                }
            }
            if matches!(
                layer.kind,
                LayerKind::Group { .. } | LayerKind::Audio { .. }
            ) && layer.anchor.is_some()
            {
                return Err(Error::InvalidShow(format!(
                    "layer {:?} has an anchor, but no content box",
                    layer.name
                )));
            }
            if let LayerKind::Image {
                tint: Some(tint), ..
            } = &layer.kind
            {
                parse_color(tint).ok_or_else(|| Error::InvalidColor(tint.clone()))?;
            }
            if let LayerKind::Audio {
                looping,
                repeat,
                delay,
                voices,
                retrigger,
                gain,
                ..
            } = &layer.kind
            {
                let problem = if *looping && repeat.is_some() {
                    Some("sets both loop and repeat")
                } else if !delay.is_finite() || *delay < 0.0 {
                    Some("needs a delay of 0 or more")
                } else if repeat.is_some_and(|r| !r.is_finite() || r < 0.0) {
                    Some("needs a repeat of 0 or more")
                } else if !gain.is_finite() || *gain < 0.0 {
                    Some("needs a gain of 0 or more")
                } else if *retrigger == Retrigger::Overlap && *voices == 0 {
                    Some("needs at least one voice to overlap")
                } else {
                    None
                };
                if let Some(problem) = problem {
                    return Err(Error::InvalidShow(format!(
                        "audio layer {:?} {problem}",
                        layer.name
                    )));
                }
            }
            layers(show, layer.children())?;
        }
        Ok(())
    }
    show.layer_trees().try_for_each(|tree| layers(show, tree))
}

/// The pixel rectangle `[x, y, width, height]` of sheet cell `frame`
/// (rounded down, clamped to the cells that fit the image).
fn sheet_cell(sheet: Sheet, image_width: u32, image_height: u32, frame: f64) -> [u32; 4] {
    let [cw, ch] = sheet.cell.map(|c| c.max(1));
    let columns = sheet.columns.clamp(1, (image_width / cw).max(1));
    let rows = (image_height / ch).max(1);
    let last = columns * rows - 1;
    let index = if frame.is_finite() && frame > 0.0 {
        (frame.floor() as u32).min(last)
    } else {
        0
    };
    [index % columns * cw, index / columns * ch, cw, ch]
}

/// Collect the paths of object keys present in `given` but absent from
/// `understood` (the same document after a round trip through the model),
/// which are the fields deserialization silently dropped.
fn ignored_fields(
    given: &serde_json::Value,
    understood: &serde_json::Value,
    path: &str,
    out: &mut Vec<String>,
) {
    use serde_json::Value as Json;
    match (given, understood) {
        (Json::Object(given), Json::Object(understood)) => {
            for (key, value) in given {
                let here = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match understood.get(key) {
                    Some(kept) => ignored_fields(value, kept, &here, out),
                    None if key.starts_with('$') => {}
                    // An explicit null carries no value to lose, and a
                    // field whose value is nothing is written back as
                    // nothing.
                    None if value.is_null() => {}
                    None => out.push(here),
                }
            }
        }
        (Json::Array(given), Json::Array(understood)) => {
            for (i, (value, kept)) in given.iter().zip(understood).enumerate() {
                ignored_fields(value, kept, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

fn layer_at<'a>(layers: &'a [Layer], path: &[usize]) -> Option<&'a Layer> {
    let (&first, rest) = path.split_first()?;
    let layer = layers.get(first)?;
    if rest.is_empty() {
        return Some(layer);
    }
    layer_at(layer.children(), rest)
}

fn collect_timelines(
    layers: &[Layer],
    path: &mut Vec<usize>,
    f: &mut impl FnMut(&[usize], usize, &Timeline),
) {
    for (i, layer) in layers.iter().enumerate() {
        path.push(i);
        for (idx, tl) in layer.timelines.iter().enumerate() {
            f(path, idx, tl);
        }
        collect_timelines(layer.children(), path, f);
        path.pop();
    }
}

/// Place a shape's local geometry: scaled uniformly around the layer's
/// x/y origin, then translated to it. A stroked rect or circle resolves
/// as a path, the one shape that carries a stroke.
fn resolve_shape(
    shape: &Shape,
    x: f64,
    y: f64,
    scale: f64,
    stroke: Option<([u8; 4], f64)>,
) -> ResolvedShape {
    let place = |[px, py]: [f64; 2]| [px * scale + x, py * scale + y];
    match (shape, stroke) {
        (Shape::Rect([rx, ry, w, h]), None) => ResolvedShape::Rect {
            x: rx * scale + x,
            y: ry * scale + y,
            width: w * scale,
            height: h * scale,
        },
        (Shape::Circle([cx, cy, r]), None) => ResolvedShape::Circle {
            cx: cx * scale + x,
            cy: cy * scale + y,
            radius: r * scale,
        },
        (Shape::Rect([rx, ry, w, h]), stroke) => ResolvedShape::Path {
            elements: [
                PathElement::MoveTo([*rx, *ry]),
                PathElement::LineTo([rx + w, *ry]),
                PathElement::LineTo([rx + w, ry + h]),
                PathElement::LineTo([*rx, ry + h]),
                PathElement::Close,
            ]
            .into_iter()
            .map(|e| e.map(place))
            .collect(),
            stroke,
        },
        (Shape::Circle([cx, cy, r]), stroke) => {
            // Four cubic quarter arcs, the usual approximation.
            const K: f64 = 0.552_284_749_8;
            let (cx, cy, r) = (*cx, *cy, *r);
            let k = K * r;
            let elements = [
                PathElement::MoveTo([cx + r, cy]),
                PathElement::CubicTo([cx + r, cy + k], [cx + k, cy + r], [cx, cy + r]),
                PathElement::CubicTo([cx - k, cy + r], [cx - r, cy + k], [cx - r, cy]),
                PathElement::CubicTo([cx - r, cy - k], [cx - k, cy - r], [cx, cy - r]),
                PathElement::CubicTo([cx + k, cy - r], [cx + r, cy - k], [cx + r, cy]),
                PathElement::Close,
            ];
            ResolvedShape::Path {
                elements: elements.into_iter().map(|e| e.map(place)).collect(),
                stroke,
            }
        }
        (Shape::Path(data), stroke) => ResolvedShape::Path {
            elements: data.elements().iter().map(|e| e.map(place)).collect(),
            stroke,
        },
    }
}

/// One paintable item of the flattened show, in canvas coordinates.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ResolvedLayer {
    pub name: String,
    pub shape: ResolvedShape,
    /// Fill color as RGBA bytes; opaque white for images.
    pub color: [u8; 4],
    /// Effective opacity in [0, 1] (tree-multiplied).
    pub opacity: f64,
    /// How the item combines with what was painted before it. Markers
    /// (clips, blend groups) carry `Normal`.
    pub blend: Blend,
    /// Applied to the shape's coordinates to place it on the canvas. The
    /// identity for anything only translated and uniformly scaled, which is
    /// then already in canvas coordinates; a rotation or an uneven scale
    /// anywhere up the tree leaves the shape in the layer's own space and
    /// puts the whole placement here. Hosts drawing the list themselves
    /// apply it (a clip's shape included).
    pub transform: Transform,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ResolvedShape {
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Circle {
        cx: f64,
        cy: f64,
        radius: f64,
    },
    /// Start clipping: until the matching [`ResolvedShape::ClipEnd`],
    /// items only show inside `shape` (a rect or circle). Clips nest.
    ClipBegin {
        shape: Box<ResolvedShape>,
    },
    /// End the innermost clip.
    ClipEnd,
    /// Start a group that is composited as one picture with `blend`, until
    /// the matching [`ResolvedShape::BlendEnd`]. Nests with clips: a
    /// clipped blended group opens the blend first.
    BlendBegin {
        blend: Blend,
    },
    /// End the innermost blend group.
    BlendEnd,
    /// A filled polygon (a segment of a segment display), closed
    /// implicitly.
    Polygon {
        points: Vec<[f64; 2]>,
    },
    /// A path of lines and curves in canvas coordinates, filled with the
    /// layer's color (a fully transparent color means no fill) and then
    /// outlined with `stroke` (color, width in canvas pixels) when given.
    /// As a clip, the stroke is ignored.
    Path {
        elements: Vec<PathElement>,
        stroke: Option<([u8; 4], f64)>,
    },
    /// A host image (look the pixels up via [`Engine::image`]) drawn into
    /// the destination rectangle: the whole image, or with `source` only
    /// that pixel rectangle `[x, y, width, height]` of it (a sheet cell).
    Image {
        image: String,
        source: Option<[u32; 4]>,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    /// Text in an outline font: fill the outlines of `glyphs` from `font`
    /// at `size` pixels per em with the layer's color, after drawing
    /// `border` (color, width in pixels) around them when given. Hosts
    /// drawing the list themselves need a font rasterizer for this; they
    /// may skip it, bitmap fonts being the portable choice.
    GlyphRun {
        font: FontData,
        size: f64,
        glyphs: Vec<PlacedGlyph>,
        border: Option<([u8; 4], f64)>,
    },
    /// Pixels the engine generated (rasterized text), drawn into the
    /// destination rectangle. `image.revision()` identifies the content.
    Bitmap {
        image: ImageData,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
}
