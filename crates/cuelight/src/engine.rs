use crate::font::{BitmapFont, Rgba, StyledFont};
use crate::lru::ByteLru;
use crate::model::{
    parse_color, Align, Binding, Blend, Choice, DigitDisplay, Justify, Layer, LayerKind, MediaKind,
    Output, Pass, Pick, Property, Retrigger, Scaling, Shape, Sheet, Show, Timeline, Triggers,
    ValueTimeline, FORMAT,
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
    #[error("invalid video: {0}")]
    InvalidVideo(String),
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
    /// Whether this change runs between whole numbers, which is what
    /// decides if a counter shows whole numbers on the way.
    ///
    /// It has to be remembered rather than read off `start`, because a
    /// change that interrupts another starts from wherever the last one
    /// had got to, which is a fraction. What matters is the values the
    /// binding was given, not where the interruption happened to land.
    whole: bool,
}

/// A color on its way to another one. The same shape as a [`Change`], and
/// eased by the same transition: one progress from 0 to 1 carries all four
/// channels, so they arrive together however the ease is shaped.
#[derive(Debug, Clone, Copy)]
struct ColorChange {
    start: [u8; 4],
    target: [u8; 4],
    started: f64,
}

impl ColorChange {
    /// Where the color has reached, `progress` of the way along.
    fn value_at(&self, progress: f64) -> [u8; 4] {
        let mut out = [0u8; 4];
        for (i, channel) in out.iter_mut().enumerate() {
            let (from, to) = (f64::from(self.start[i]), f64::from(self.target[i]));
            *channel = (from + (to - from) * progress).round().clamp(0.0, 255.0) as u8;
        }
        out
    }
}

/// A color as a show writes one, so a transition's value is a value like
/// any other.
fn color_text([r, g, b, a]: [u8; 4]) -> String {
    match a {
        255 => format!("#{r:02X}{g:02X}{b:02X}"),
        _ => format!("#{r:02X}{g:02X}{b:02X}{a:02X}"),
    }
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

/// What a playhead's timeline belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    /// A layer's timeline, animating that layer's properties.
    Layer { root: Root, path: Vec<usize> },
    /// A show value's timeline, animating the value itself.
    Value(String),
}

impl Owner {
    /// Which tree it lives in. A value belongs to the show, so leaving a
    /// scene does not stop one.
    fn root(&self) -> Root {
        match self {
            Owner::Layer { root, .. } => *root,
            Owner::Value(_) => Root::Show,
        }
    }

    /// Whether it is the timeline of exactly this layer.
    fn is_layer(&self, root: Root, path: &[usize]) -> bool {
        matches!(self, Owner::Layer { root: r, path: p } if *r == root && p == path)
    }
}

/// A timeline's timing, whatever it animates: what the clock needs from
/// a layer's timeline and from a value's alike.
#[derive(Debug, Clone, Copy)]
struct Timing<'a> {
    duration: f64,
    play_time: f64,
    looping: bool,
    hold: bool,
    on_end: Option<&'a str>,
}

impl<'a> From<&'a Timeline> for Timing<'a> {
    fn from(tl: &'a Timeline) -> Self {
        Timing {
            duration: tl.duration(),
            play_time: tl.play_time(),
            looping: tl.looping,
            hold: tl.hold,
            on_end: tl.on_end.as_deref(),
        }
    }
}

impl<'a> From<&'a ValueTimeline> for Timing<'a> {
    fn from(tl: &'a ValueTimeline) -> Self {
        Timing {
            duration: tl.duration(),
            play_time: tl.play_time(),
            looping: tl.looping,
            hold: tl.hold,
            on_end: tl.on_end.as_deref(),
        }
    }
}

/// Which timelines a start is asking for.
#[derive(Debug, Clone, Copy)]
enum Want<'a> {
    /// Everything that starts when the show loads or a scene is entered.
    Autoplay,
    /// Everything this trigger starts.
    Trigger(&'a str),
}

impl Want<'_> {
    fn picks(self, autoplay: bool, trigger: &Triggers) -> bool {
        match self {
            Want::Autoplay => autoplay,
            Want::Trigger(name) => trigger.contains(name),
        }
    }
}

/// The timing of whatever `p` is playing, from the loaded show.
///
/// Free rather than a method so it can be called while the playheads are
/// borrowed.
fn timing_in<'a>(show: &'a Show, p: &Playhead) -> Option<Timing<'a>> {
    match &p.owner {
        Owner::Layer { root, path } => root_layers(show, *root)
            .and_then(|layers| layer_at(layers, path))
            .and_then(|layer| layer.timelines.get(p.timeline))
            .map(Timing::from),
        Owner::Value(name) => show
            .values
            .get(name)
            .and_then(|value| value.timelines.get(p.timeline))
            .map(Timing::from),
    }
}

/// Passes a segment's halo is drawn in, each wider and fainter than the
/// last.
///
/// Few enough steps and a wide halo reads as stacked outlines rather than
/// a spill of light; the blotchiness is the steps showing. Eight is where
/// they stop being visible at the widest halo the format allows, and a
/// display of six digits still resolves in tens of microseconds.
const GLOW_STEPS: u32 = 8;

/// A running timeline instance.
#[derive(Debug, Clone)]
struct Playhead {
    owner: Owner,
    /// Timeline index within its owner.
    timeline: usize,
    /// The instant on the show's clock the timeline's first key falls on:
    /// when it was started, plus its delay. A playhead keeps no clock of
    /// its own; where it is is worked out from this and the show's clock,
    /// so however long a chain of `on_end` runs for, the two can never
    /// drift apart.
    starts: f64,
    /// Finished, but holding its properties at their last values.
    held: bool,
}

impl Playhead {
    /// Seconds since the delay ended at show time `now`: negative while
    /// still delayed, and wrapped for a `loop`.
    fn at(&self, now: f64, tl: Timing<'_>) -> f64 {
        if self.held {
            return tl.play_time;
        }
        let mut elapsed = now - self.starts;
        // An anchor a hair ahead of the clock is one that has just
        // started, not one still waiting: the same instant, as in every
        // other comparison. Without this a link handed the property over
        // a rounding error early owns nothing for a frame, and the layer
        // falls back to its base value for it.
        if elapsed < 0.0 && elapsed > -SAME_INSTANT {
            elapsed = 0.0;
        }
        if elapsed > 0.0 && tl.looping && tl.duration > 0.0 {
            return elapsed % tl.duration;
        }
        elapsed
    }

    /// The instant it finishes, for a timeline that does finish.
    fn ends(&self, tl: Timing<'_>) -> f64 {
        self.starts + tl.play_time
    }
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
    /// What it is playing. A video layer's name can be bound, and
    /// pointing it at another clip starts that one from the top.
    playing: String,
}

/// Where a ducking layer's level is and when it started going there.
#[derive(Debug, Clone, Copy)]
struct Ducked {
    /// Whether the bus it listens to was sounding at the last step.
    down: bool,
    /// Engine time the level started moving toward where it is going.
    since: f64,
    /// The level it was at when it started moving, so a ramp interrupted
    /// halfway carries on from where it is rather than jumping.
    from: f64,
}

/// What the engine knows of a video: how long it runs and how big it is.
/// The frames are the host's business.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    /// Seconds, for looping, repeating and ending a play.
    pub duration: f64,
    /// The video's own size in pixels, what a layer without `size` draws
    /// at.
    pub width: f64,
    pub height: f64,
}

/// A video that should be showing now, as [`Engine::videos`] reports it:
/// the picture twin of a [`Voice`]. A host decodes to `position` and hands
/// the frame back with
/// [`set_image`](Engine::set_image) under the video's name.
#[derive(Debug, Clone, PartialEq)]
pub struct Playing {
    /// Identifies one play for as long as it lasts; never reused.
    pub id: u64,
    /// Name of the video layer.
    pub layer: String,
    /// The video, as registered with [`Engine::set_video`].
    pub video: String,
    /// The name to hand this play's picture over under, with
    /// [`Engine::set_image`], and the one the layer draws from.
    ///
    /// One per video layer, not per clip: two layers playing one clip at
    /// different positions each show their own frame, where a name they
    /// shared would leave both drawing whichever was written last. The
    /// engine makes it; a host only passes it back.
    pub frame: String,
    /// Seconds into the video, wrapped for loops and repeats.
    pub position: f64,
    /// Whether it plays on from its end.
    pub looping: bool,
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

/// How many times one frame may be split at something ending.
///
/// A frame holds as many endings as the show puts in it, and each is
/// worth stopping at, so this has to be far above anything a show would
/// ask for: one long frame at a low frame rate can hold hundreds of
/// links of a chain, and capping it would make the frame rate part of
/// the answer, which is the one thing the clock must not depend on.
///
/// What it guards against is a show whose links are shorter than
/// [`SAME_INSTANT`], where a frame could be split until it ran out of
/// floats. Past this the rest of the frame is taken in one piece and the
/// chain carries on next frame, which is no longer exact; a show that
/// reaches it is asking for more than a hundred thousand endings inside
/// one frame.
const MAX_SUBSTEPS: usize = 100_000;

/// What is sounding on each bus, by the layer playing it and the instant
/// that play started sounding.
type BusyBuses = std::collections::BTreeMap<String, Vec<((Root, Vec<usize>), f64)>>;

/// Two instants closer together than this are the same instant.
///
/// Time is seconds in an `f64`, so the same instant reached two ways --
/// counting frames of a sixtieth, or adding up three clips of 0.7 s --
/// comes out a few parts in 10^15 apart. Without this a clip whose end
/// lands a femtosecond past a frame boundary waits a whole frame. A
/// microsecond is far under anything a show can ask for or a frame can
/// resolve, and far over that noise at any show length.
const SAME_INSTANT: f64 = 1e-6;

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
    /// Registered videos: what the engine knows without decoding them.
    videos: BTreeMap<String, VideoInfo>,
    playing: Vec<Playhead>,
    sounding: Vec<Sounding>,
    /// Id for the next play; ids are never reused.
    next_voice: u64,
    /// Every binding with a transition, found once at load.
    transition_sites: Vec<TransitionSite>,
    /// The change each of them is making; absent until its first frame,
    /// so a property starts at its value instead of easing in.
    transitions: HashMap<TransitionSite, Change>,
    /// The same, for the one property whose value is a color.
    color_transitions: HashMap<TransitionSite, ColorChange>,
    /// Every binding with a debounce, found once at load, and what each
    /// has settled on.
    debounce_sites: Vec<TransitionSite>,
    debounced: HashMap<TransitionSite, Settling>,
    /// Where each ducking layer's level is: whether its bus was sounding
    /// at the last step and when that last changed, so a ramp knows where
    /// it started.
    ducking: HashMap<(Root, Vec<usize>), Ducked>,
    /// Which timeline conditions held last frame, so becoming true can be
    /// told from staying true.
    conditions: HashMap<(Root, Vec<usize>, usize), bool>,
    /// What each pointed layer last played, so pointing one somewhere new
    /// can be told from one that simply finished.
    shown: HashMap<(Root, Vec<usize>), String>,
    /// What each playhead has played so far: how many times, which is
    /// what decides which of several assets the next play takes, and when
    /// the last one started, for `rest`.
    plays: HashMap<(Root, Vec<usize>), Played>,
    /// Plays waiting their turn, oldest first; see
    /// [`Retrigger::Queue`](crate::model::Retrigger::Queue). Each holds
    /// the asset asked for, so a queue of different clips stays a queue
    /// of different clips.
    waiting: Vec<(Root, Vec<usize>, Option<String>)>,
    /// Scrambles the picks that are meant to vary; see
    /// [`Engine::set_seed`].
    seed: u64,
    /// Every reel row, found once at load, and where each of its cells is
    /// on its ring: one record per cell, in cell order.
    reel_sites: Vec<(Root, Vec<usize>)>,
    reels: HashMap<(Root, Vec<usize>), Vec<Change>>,
    /// Reel rows told to spin since the last frame; see
    /// [`Reel::spin`](crate::model::Reel::spin).
    spinning: Vec<(Root, Vec<usize>)>,
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
        quiet_bindings(&show, &mut self.load_warnings);
        (self.transition_sites, self.debounce_sites) = binding_sites(&show);
        self.reel_sites = reel_sites(&show);
        self.show = Some(show);
        self.restart();
        Ok(())
    }

    /// Put the loaded show back to its beginning: time 0, the first
    /// scene, nothing playing, every variable at the value the document
    /// declares.
    ///
    /// Registered assets are host state and are left alone, so this is
    /// cheap: no file is read and nothing is decoded again. Reaching a
    /// moment is therefore restarting and advancing to it, which is what
    /// lets a host scrub without the engine holding any history.
    ///
    /// Does nothing without a show.
    pub fn restart(&mut self) {
        let Some(show) = &self.show else { return };
        self.variables = show.variables.clone();
        let scenes = !show.scenes.is_empty();
        self.playing.clear();
        self.sounding.clear();
        self.transitions.clear();
        self.color_transitions.clear();
        self.debounced.clear();
        self.reels.clear();
        self.spinning.clear();
        self.shown.clear();
        self.ducking.clear();
        self.conditions.clear();
        self.plays.clear();
        self.waiting.clear();
        self.events.clear();
        self.time = 0.0;
        self.active_scene = scenes.then_some(0);
        self.start_matching(Some(Root::Show), self.time, Want::Autoplay);
        self.play_autoplay(Root::Show);
        if let Some(scene) = self.active_scene {
            self.start_matching(Some(Root::Scene(scene)), self.time, Want::Autoplay);
            self.play_autoplay(Root::Scene(scene));
        }
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

    /// What `name` reads as now: the host's variable of that name, or
    /// failing that the show's own value of it.
    ///
    /// A host that sets a variable takes the show's value over, so a show
    /// can ship with its own motion that a host is free to seize.
    pub fn value(&self, name: &str) -> Option<Value> {
        self.variables
            .get(name)
            .cloned()
            .or_else(|| self.show_value(name).map(Value::Number))
    }

    /// The number a show value stands at now, from whichever of its
    /// timelines is running: a held one only if nothing else is, as a
    /// layer's properties resolve.
    fn show_value(&self, name: &str) -> Option<f64> {
        let show = self.show.as_ref()?;
        let value = show.values.get(name)?;
        let mut out = None;
        for running in [false, true] {
            for p in self.playing.iter().filter(|p| p.held != running) {
                if !matches!(&p.owner, Owner::Value(v) if v == name) {
                    continue;
                }
                let Some(tl) = value.timelines.get(p.timeline) else {
                    continue;
                };
                if let Some(v) = tl.at(p.at(self.time, tl.into())) {
                    out = Some(v);
                }
            }
        }
        out
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

    /// Register (or replace) a named video by its `duration` in seconds
    /// and its size in pixels, which is all the engine needs of it: to
    /// loop, repeat and end plays, and to lay its layer out. Decoding is
    /// the host's: it reads [`videos`](Engine::videos) each frame and
    /// hands the picture back with [`set_image`](Engine::set_image) under
    /// the same name, which the video layer draws. Like images, videos
    /// survive `load_show` and may arrive after it.
    pub fn set_video(
        &mut self,
        name: &str,
        duration: f64,
        [width, height]: [f64; 2],
    ) -> Result<(), Error> {
        let sane = |n: f64| n.is_finite() && n > 0.0;
        if !sane(duration) || !sane(width) || !sane(height) {
            return Err(Error::InvalidVideo(format!(
                "{name:?}: {duration}s at {width}x{height} is not above 0"
            )));
        }
        self.videos.insert(
            name.to_owned(),
            VideoInfo {
                duration,
                width,
                height,
            },
        );
        Ok(())
    }

    /// Scramble the picks that are meant to vary.
    ///
    /// A layer that names several assets with `pick: random` or
    /// `pick: shuffle` picks by counting its plays, not by rolling dice,
    /// so a show plays the same way every run. That is what rendering a
    /// show to a file needs, and it is the wrong thing for a show that
    /// runs all day: seed the engine with something that differs per run
    /// (the clock will do) and the same show varies between runs while
    /// staying repeatable within one.
    ///
    /// Set it before the show loads; it changes nothing already played.
    pub fn set_seed(&mut self, seed: u64) {
        self.seed = seed;
    }

    /// What is registered for video `name`.
    pub fn video(&self, name: &str) -> Option<VideoInfo> {
        self.videos.get(name).copied()
    }

    /// How long the content of a playhead runs, whichever registry it
    /// comes from; `None` while the host has not registered it.
    fn media_duration(&self, media: &crate::model::Media<'_>, playing: &str) -> Option<f64> {
        // What it is playing, which a binding or a pick may have chosen.
        let name = if playing.is_empty() {
            media.names.first()
        } else {
            playing
        };
        match media.kind {
            MediaKind::Sound => self.sounds.get(name).copied(),
            MediaKind::Video => self.videos.get(name).map(|video| video.duration),
        }
    }

    /// The videos that should be showing now, in tree order: every play of
    /// a visible video layer whose video is registered and whose delay is
    /// over, at its position. The picture twin of
    /// [`voices`](Engine::voices); a host decodes to these positions.
    pub fn videos(&self) -> Result<Vec<Playing>, Error> {
        let show = self.show.as_ref().ok_or(Error::NoShow)?;
        let mut out = Vec::new();
        for (root, layers) in std::iter::once((Root::Show, show.layers.as_slice())).chain(
            self.active_scene
                .and_then(|i| Some((Root::Scene(i), root_layers(show, Root::Scene(i))?))),
        ) {
            self.watch(root, layers, &mut Vec::new(), &mut out);
        }
        Ok(out)
    }

    fn watch(&self, root: Root, layers: &[Layer], path: &mut Vec<usize>, out: &mut Vec<Playing>) {
        for (i, layer) in layers.iter().enumerate() {
            path.push(i);
            if self.is_visible(root, layer, path) {
                if let LayerKind::Video { .. } = &layer.kind {
                    let media = layer.kind.media();
                    let plays = self
                        .sounding
                        .iter()
                        .filter(|s| s.root == root && s.layer_path == *path);
                    for play in plays {
                        let video = &play.playing;
                        let (Some(media), Some(info)) = (media, self.videos.get(video)) else {
                            continue;
                        };
                        let elapsed = self.time - play.started - media.delay.max(0.0);
                        if elapsed < 0.0 {
                            continue;
                        }
                        let position = if media.looping || media.repeat.is_some() {
                            elapsed % info.duration
                        } else {
                            elapsed
                        };
                        out.push(Playing {
                            id: play.id,
                            layer: layer.name.clone(),
                            video: video.clone(),
                            frame: frame_key(root, path),
                            position,
                            looping: media.looping,
                        });
                    }
                }
                self.watch(root, layer.children(), path, out);
            }
            path.pop();
        }
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
        self.trigger_at(name, self.time);
    }

    /// Fire `name` as if at the instant `at`, which is what whatever it
    /// starts is timed from.
    ///
    /// For a trigger from the host that instant is the clock. For an
    /// `on_end` it is the instant the thing that fired it finished, which
    /// is not quite the clock when the frame had to land a hair short of
    /// it; anchoring there is what stops a chain's slack adding up.
    fn trigger_at(&mut self, name: &str, at: f64) {
        let entered = self
            .show
            .as_ref()
            .and_then(|show| show.scenes.iter().position(|s| s.trigger.contains(name)));
        if let Some(scene) = entered {
            self.enter_scene(scene);
        }
        self.start_matching(None, at, Want::Trigger(name));
        self.set_spinning(name);
        let roots: Vec<Root> = std::iter::once(Root::Show)
            .chain(self.active_scene.map(Root::Scene))
            .collect();
        for root in roots {
            for (path, (stops, plays)) in self.media_layers(root, |trigger, stop| {
                (stop.contains(name), trigger.contains(name))
            }) {
                if stops {
                    self.sounding
                        .retain(|s| !(s.root == root && s.layer_path == path));
                    // Whatever was waiting its turn is not owed a turn.
                    self.waiting.retain(|(r, p, _)| !(*r == root && *p == path));
                }
                if plays {
                    self.play(root, path, at);
                }
            }
        }
    }

    /// The layers pointed at media they are not playing: idle ones whose
    /// bound name has changed since they last played.
    fn repointed(&self) -> Vec<(Root, Vec<usize>)> {
        let Some(show) = &self.show else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let roots = std::iter::once(Root::Show).chain(self.active_scene.map(Root::Scene));
        for root in roots {
            let Some(layers) = root_layers(show, root) else {
                continue;
            };
            let mut paths = Vec::new();
            fn walk(layers: &[Layer], path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
                for (i, layer) in layers.iter().enumerate() {
                    path.push(i);
                    if matches!(
                        layer.kind,
                        LayerKind::Video { .. } | LayerKind::Audio { .. }
                    ) {
                        out.push(path.clone());
                    }
                    walk(layer.children(), path, out);
                    path.pop();
                }
            }
            walk(layers, &mut Vec::new(), &mut paths);
            for path in paths {
                let idle = !self
                    .sounding
                    .iter()
                    .any(|play| play.root == root && play.layer_path == path);
                if !idle {
                    continue;
                }
                let Some(layer) = layer_at(layers, &path) else {
                    continue;
                };
                // Only a layer that is pointed somewhere: one playing
                // through a list of its own waits to be told to play.
                let Some(now) = self.pointed_at(root, layer, &path) else {
                    continue;
                };
                // The clip it last finished: it stays as it is until it
                // is pointed somewhere new. Never having played counts as
                // somewhere new, so the first name a host gives a surface
                // starts it like every name after.
                let shown = self.shown.get(&(root, path.clone()));
                if !now.is_empty() && shown.is_none_or(|last| *last != now) {
                    out.push((root, path));
                }
            }
        }
        out
    }

    /// The asset a new play of the layer at `path` would take: what the
    /// layer is pointed at, or the next of the several it names.
    fn media_name(&self, root: Root, path: &[usize]) -> String {
        let layer = self
            .show
            .as_ref()
            .and_then(|show| root_layers(show, root))
            .and_then(|layers| layer_at(layers, path));
        let Some(layer) = layer else {
            return String::new();
        };
        if let Some(pointed) = self.pointed_at(root, layer, path) {
            return pointed;
        }
        let Some(media) = layer.kind.media() else {
            return String::new();
        };
        let ordinal = self
            .plays
            .get(&(root, path.to_vec()))
            .map_or(0, |played| played.count);
        pick_one(
            media.names,
            media.pick,
            ordinal,
            self.seed ^ seed_of(root, path),
        )
    }

    /// What the playhead at `path` does when asked to play while it is
    /// already playing.
    fn retrigger_of(&self, root: Root, path: &[usize]) -> Retrigger {
        self.media_at(root, path)
            .map_or(Retrigger::Restart, |media| media.retrigger)
    }

    /// How many plays the playhead at `path` may hold at once.
    fn voices_of(&self, root: Root, path: &[usize]) -> usize {
        self.media_at(root, path)
            .map_or(1, |media| media.voices.max(1) as usize)
    }

    /// The playhead of the layer at `path`, if it has one.
    fn media_at(&self, root: Root, path: &[usize]) -> Option<crate::model::Media<'_>> {
        self.show
            .as_ref()
            .and_then(|show| root_layers(show, root))
            .and_then(|layers| layer_at(layers, path))
            .and_then(|layer| layer.kind.media())
    }

    /// Where the layer at `path` is pointed, by path alone.
    fn pointed(&self, root: Root, path: &[usize]) -> Option<String> {
        let layer = self
            .show
            .as_ref()
            .and_then(|show| root_layers(show, root))
            .and_then(|layers| layer_at(layers, path))?;
        self.pointed_at(root, layer, path)
    }

    /// What a binding has pointed this layer at, if one has.
    ///
    /// `None` covers two cases that have to stay apart: a layer with no
    /// video binding at all, which plays a list of its own, and one whose
    /// binding has nothing to say yet, because its variable is unset or
    /// its map does not list the value. Neither has been told what to
    /// show, and a layer that has not been told does not play. Falling
    /// back to the layer's own `video` here would make those look like an
    /// instruction to show it.
    fn pointed_at(&self, root: Root, layer: &Layer, path: &[usize]) -> Option<String> {
        let mut pointed = None;
        for (index, b) in layer.bindings.iter().enumerate() {
            // Whichever of the two names a playhead's media; a layer can
            // only carry the one its kind has.
            if !matches!(b.property, Property::Video | Property::Sound) {
                continue;
            }
            let bound = self.in_transition(root, path, index, b).or_else(|| {
                self.binding_value(&(root, path.to_vec(), index), b)
                    .and_then(|value| self.convert(b, value))
            });
            if let Some(bound) = bound {
                pointed = Some(bound.to_text());
            }
        }
        pointed
    }

    /// The asset the layer at `path` is showing: what its running play
    /// took, or what a new play would take while nothing runs.
    fn showing(&self, root: Root, path: &[usize]) -> String {
        self.sounding
            .iter()
            .find(|s| s.root == root && s.layer_path == *path)
            .map_or_else(|| self.media_name(root, path), |s| s.playing.clone())
    }

    /// The layers of `root` with a playhead for which `want` (given their
    /// `trigger` and `stop`) says something: their paths, with what it
    /// said.
    fn media_layers<T>(
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
                if let Some(media) = layer.kind.media() {
                    out.push((path.clone(), want(media.trigger, media.stop)));
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

    /// Start the autoplay sounds and videos of `root`.
    fn play_autoplay(&mut self, root: Root) {
        let Some(layers) = self.show.as_ref().and_then(|show| root_layers(show, root)) else {
            return;
        };
        let mut starts = Vec::new();
        fn walk(layers: &[Layer], path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            for (i, layer) in layers.iter().enumerate() {
                path.push(i);
                if layer.kind.media().is_some_and(|media| media.autoplay) {
                    out.push(path.clone());
                }
                walk(layer.children(), path, out);
                path.pop();
            }
        }
        walk(layers, &mut Vec::new(), &mut starts);
        for path in starts {
            self.play(root, path, self.time);
        }
    }

    /// Play the audio layer at `path`, as its `retrigger` says when it
    /// already plays.
    fn play(&mut self, root: Root, path: Vec<usize>, at: f64) {
        self.start(root, path, None, at);
    }

    /// Start a play of the layer at `path`, of `asked` when the caller
    /// has already settled which asset it wants.
    fn start(&mut self, root: Root, path: Vec<usize>, asked: Option<String>, at: f64) {
        let layer = self
            .show
            .as_ref()
            .and_then(|show| root_layers(show, root))
            .and_then(|layers| layer_at(layers, &path));
        let Some(media) = layer.and_then(|layer| layer.kind.media()) else {
            return;
        };
        let (retrigger, voices) = (media.retrigger, media.voices as usize);
        // Too soon after the last play: dropped, whatever the layer would
        // otherwise do with it.
        if media.rest > 0.0 {
            let last = self.plays.get(&(root, path.clone())).map(|p| p.at);
            if last.is_some_and(|last| at - last < media.rest) {
                return;
            }
        }
        let mine = |s: &Sounding| s.root == root && s.layer_path == path;
        match retrigger {
            Retrigger::Restart => self.sounding.retain(|s| !mine(s)),
            Retrigger::Ignore if self.sounding.iter().any(mine) => return,
            Retrigger::Ignore => {}
            Retrigger::Queue if self.sounding.iter().any(mine) => {
                // In line behind what is playing, and behind whatever is
                // already waiting. Beyond the layer's `voices` the
                // trigger is dropped rather than piling up.
                let waiting = self
                    .waiting
                    .iter()
                    .filter(|(r, p, _)| *r == root && *p == path)
                    .count();
                if waiting < voices.max(1) {
                    let asked = self.media_name(root, &path);
                    self.waiting.push((root, path, Some(asked)));
                }
                return;
            }
            Retrigger::Queue => {}
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
        let playing = asked.unwrap_or_else(|| self.media_name(root, &path));
        self.shown.insert((root, path.clone()), playing.clone());
        let played = self.plays.entry((root, path.clone())).or_default();
        played.count += 1;
        played.at = at;
        self.sounding.push(Sounding {
            root,
            layer_path: path,
            id: self.next_voice,
            started: at,
            playing,
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
        self.playing.retain(|p| p.owner.root() == Root::Show);
        // Leaving a scene stops its sounds.
        self.sounding.retain(|s| s.root == Root::Show);
        self.waiting.retain(|(root, ..)| *root == Root::Show);
        // A scene's properties start at their values, like at load.
        self.transitions.retain(|(root, ..), _| *root == Root::Show);
        self.color_transitions
            .retain(|(root, ..), _| *root == Root::Show);
        self.debounced.retain(|(root, ..), _| *root == Root::Show);
        self.reels.retain(|(root, ..), _| *root == Root::Show);
        self.active_scene = Some(scene);
        self.start_matching(Some(Root::Scene(scene)), self.time, Want::Autoplay);
        self.play_autoplay(Root::Scene(scene));
    }

    /// Advance time by `dt` seconds: running timelines and sounds
    /// progress, looping ones wrap, finished ones stop (their properties
    /// fall back to bindings/base values) and fire their `on_end` trigger,
    /// which is also reported through [`drain_events`](Engine::drain_events).
    ///
    /// A frame is not one step. Anything that ends inside it ends at the
    /// instant it ends, not at the end of the frame, so a chain of
    /// timelines linked by `on_end` keeps the schedule its durations
    /// describe whatever the frame rate is. The host still hears about
    /// the event when the frame returns; the show's own clocks are
    /// already right.
    pub fn advance_frame(&mut self, dt: f64) {
        self.advance_to(self.time + dt);
    }

    /// Advance to the instant `to`, which is where the clock lands.
    ///
    /// The same as [`advance_frame`](Engine::advance_frame) except in
    /// what it is told. A host that knows what time it is -- replaying a
    /// script, rendering chosen moments, seeking -- should say so: a
    /// delta has to be worked out from where the clock already is, and
    /// `previous + delta` is not the instant that was meant, so the same
    /// moment reached at two frame rates lands a rounding error apart.
    /// Told the instant, the clock is exactly it.
    ///
    /// Going backwards does nothing; a show is walked forwards from its
    /// start. Landing where the clock already is runs a step without
    /// moving it, which is how a host settles a show it has just poked.
    pub fn advance_to(&mut self, to: f64) {
        if to < self.time {
            return;
        }
        let end = to;
        // Each pass runs to the first thing that ends, so the pass after
        // it starts exactly where that one finished.
        for _ in 0..MAX_SUBSTEPS {
            let to = self.next_change(end);
            self.step(to);
            if self.time >= end {
                return;
            }
        }
        // A show whose chain is all zero-length timelines would split a
        // frame for ever; it gets the rest of the frame in one piece.
        self.step(end);
    }

    /// The instant something next changes, or `end` if nothing does
    /// before then.
    ///
    /// Every end is an instant on the show's clock, never a countdown:
    /// the same expression picks the instant here and recognises it in
    /// [`Self::step`], so a step that lands on it cannot land a float's
    /// width short and put the ending off to the next frame.
    ///
    /// Only what is already running counts: whatever an `on_end` starts is
    /// looked at on the next pass, having begun at the right instant.
    fn next_change(&self, end: f64) -> f64 {
        let Some(show) = &self.show else {
            return end;
        };
        let mut first = end;
        for p in &self.playing {
            if p.held {
                continue;
            }
            let Some(tl) = timing_in(show, p) else {
                continue;
            };
            if tl.looping || tl.duration <= 0.0 {
                continue;
            }
            let ends = p.ends(tl);
            if ends > self.time + SAME_INSTANT && ends < first {
                first = ends;
            }
        }
        for play in &self.sounding {
            let layer = root_layers(show, play.root).and_then(|l| layer_at(l, &play.layer_path));
            let Some(media) = layer.and_then(|l| l.kind.media()) else {
                continue;
            };
            if media.looping {
                continue;
            }
            let Some(length) = self.media_duration(&media, &play.playing) else {
                continue;
            };
            let plays = media.repeat.unwrap_or(1.0).max(0.0);
            let starts = play.started + media.delay.max(0.0);
            // When it starts sounding, as well as when it stops. A play
            // waiting out a delay is not on its bus yet, so anything
            // ducking under that bus turns at this instant; without it a
            // frame could span a short delayed clip from before it was
            // audible to after it was over, and nothing would duck.
            if starts > self.time + SAME_INSTANT && starts < first {
                first = starts;
            }
            let ends = starts + length * plays;
            if ends > self.time + SAME_INSTANT && ends < first {
                first = ends;
            }
        }
        first
    }

    /// One indivisible move of the clock, landing exactly on `to`; see
    /// [`Self::advance_frame`].
    ///
    /// It is given where to land, never how far to go: everything inside
    /// is worked out from instants on the clock, so a step cannot leave
    /// anything a fraction of a frame away from where the show says it
    /// should be.
    fn step(&mut self, to: f64) {
        // Also before the step, not only after it: a play triggered
        // between two frames starts the bus at the instant the step
        // begins, and a long frame would otherwise see it begin and end
        // without ever noticing it sounded.
        self.follow_ducks();
        self.settle_debounces(to);
        self.follow_conditions();
        self.follow_transitions();
        self.follow_reels();
        // A layer pointed at another clip shows that one, from the top,
        // whether or not it was showing anything before: that is what an
        // event asking for a clip means.
        let pointed: Vec<(usize, String)> = self
            .sounding
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let now = self.pointed(s.root, &s.layer_path)?;
                (now != s.playing).then_some((i, now))
            })
            .collect();
        for (i, now) in pointed {
            // Being pointed somewhere new is a play like any other, so
            // the layer's `retrigger` says what happens to the one that
            // is running.
            let (root, path) = (self.sounding[i].root, self.sounding[i].layer_path.clone());
            match self.retrigger_of(root, &path) {
                Retrigger::Ignore => continue,
                Retrigger::Queue => {
                    // Pointing a layer somewhere is one ask, however many
                    // frames it stays pointed there, so it takes one place
                    // in the queue: the name is already last in line.
                    let mine: Vec<_> = self
                        .waiting
                        .iter()
                        .filter(|(r, p, _)| *r == root && *p == path)
                        .collect();
                    let asked = mine
                        .last()
                        .is_some_and(|(.., name)| name.as_deref() == Some(now.as_str()));
                    let waiting = mine.len();
                    if !asked && waiting < self.voices_of(root, &path) {
                        self.waiting.push((root, path, Some(now)));
                    }
                }
                _ => {
                    let play = &mut self.sounding[i];
                    play.playing = now.clone();
                    play.started = self.time;
                    self.next_voice += 1;
                    play.id = self.next_voice;
                    // Being pointed somewhere new in place is still a
                    // play of that clip: without this the layer would be
                    // told to start it again the moment it ended.
                    self.shown.insert((root, path), now);
                }
            }
        }
        for (root, path) in self.repointed() {
            self.play(root, path, self.time);
        }
        self.time = to;
        let now = self.time;
        let Some(show) = &self.show else { return };
        let mut finished: Vec<usize> = Vec::new();
        // Each with the instant the thing that fired it finished, not
        // the clock: that is what the next link in a chain is timed from.
        let mut on_end: Vec<(String, f64)> = Vec::new();
        for (i, p) in self.playing.iter_mut().enumerate() {
            let Some(tl) = timing_in(show, p) else {
                finished.push(i);
                continue;
            };
            // A held one is done moving: it stays where it stopped.
            if p.held {
                continue;
            }
            if now + SAME_INSTANT < p.starts {
                continue;
            }
            if tl.looping && tl.duration > 0.0 {
                continue;
            }
            // Compared as instants on the one clock, the same way the
            // step that landed here was chosen.
            if tl.duration <= 0.0 || now + SAME_INSTANT >= p.ends(tl) {
                let ends = if tl.duration <= 0.0 { now } else { p.ends(tl) };
                on_end.extend(tl.on_end.map(|name| (name.to_owned(), ends)));
                // Holding is not playing: it ends, fires its `on_end` once
                // like any other, and then keeps its last values.
                if tl.hold && !tl.looping && tl.duration > 0.0 {
                    p.held = true;
                } else {
                    finished.push(i);
                }
            }
        }
        for i in finished.into_iter().rev() {
            self.playing.remove(i);
        }
        // A play ends when its time is up; a play of an unregistered sound
        // has no end yet.
        let time = self.time;
        let mut ended: Vec<usize> = Vec::new();
        // The instant each layer fell idle, for whatever is queued behind
        // it: its turn starts where the play before it stopped.
        let mut freed: Vec<(Root, Vec<usize>, f64)> = Vec::new();
        for (i, s) in self.sounding.iter().enumerate() {
            let layer = root_layers(show, s.root).and_then(|l| layer_at(l, &s.layer_path));
            let Some(media) = layer.and_then(|layer| layer.kind.media()) else {
                ended.push(i);
                continue;
            };
            if media.looping {
                continue;
            }
            // Content the host has not registered yet has no end.
            let Some(duration) = self.media_duration(&media, &s.playing) else {
                continue;
            };
            let plays = media.repeat.unwrap_or(1.0).max(0.0);
            let ends = s.started + media.delay.max(0.0) + duration * plays;
            if time + SAME_INSTANT >= ends {
                ended.push(i);
                freed.push((s.root, s.layer_path.clone(), ends));
                on_end.extend(media.on_end.map(|name| (name.to_owned(), ends)));
            }
        }
        for i in ended.into_iter().rev() {
            self.sounding.remove(i);
        }
        // A layer that has just fallen idle takes the next play waiting
        // for it, in the order the triggers arrived.
        let mut turn: Vec<(Root, Vec<usize>, Option<String>)> = Vec::new();
        self.waiting.retain(|(root, path, asked)| {
            let busy = self
                .sounding
                .iter()
                .any(|s| s.root == *root && s.layer_path == *path);
            let taken = turn.iter().any(|(r, p, _)| r == root && p == path);
            if busy || taken {
                return true;
            }
            turn.push((*root, path.clone(), asked.clone()));
            false
        });
        for (root, path, asked) in turn {
            let at = freed
                .iter()
                .find(|(r, p, _)| *r == root && *p == path)
                .map_or(self.time, |(.., ends)| *ends);
            self.start(root, path, asked, at);
        }
        for (name, at) in on_end {
            self.trigger_at(&name, at);
            if self.events.len() == MAX_PENDING_EVENTS {
                self.events.pop_front();
            }
            self.events.push_back(Event::Trigger(name));
        }
        self.follow_ducks();
    }

    /// Note, for every layer that ducks, whether the bus it listens to is
    /// sounding now, and when that last changed.
    ///
    /// Only the change is remembered. What the level *is* at any moment is
    /// a function of that and of the time, which is what keeps it seekable:
    /// nothing here accumulates frame by frame.
    fn follow_ducks(&mut self) {
        type Found = Vec<((Root, Vec<usize>), Option<f64>)>;
        let Some(show) = &self.show else { return };
        let busy = self.busy_buses();
        let mut found: Found = Vec::new();
        let roots = std::iter::once(Root::Show).chain(self.active_scene.map(Root::Scene));
        for root in roots {
            let Some(layers) = root_layers(show, root) else {
                continue;
            };
            type Busy = BusyBuses;
            fn walk(
                root: Root,
                layers: &[Layer],
                path: &mut Vec<usize>,
                busy: &Busy,
                out: &mut Vec<(Vec<usize>, Option<f64>)>,
            ) {
                for (i, layer) in layers.iter().enumerate() {
                    path.push(i);
                    if let LayerKind::Audio {
                        duck: Some(duck), ..
                    }
                    | LayerKind::Video {
                        duck: Some(duck), ..
                    } = &layer.kind
                    {
                        // Its own plays do not duck it, so a layer on the
                        // bus it listens to is not forever out of its own
                        // way. The instant the first of them started is
                        // when the bus became busy.
                        let since = busy.get(&duck.under).and_then(|plays| {
                            plays
                                .iter()
                                .filter(|((r, p), _)| !(*r == root && p == path))
                                .map(|(_, at)| *at)
                                .min_by(f64::total_cmp)
                        });
                        out.push((path.clone(), since));
                    }
                    walk(root, layer.children(), path, busy, out);
                    path.pop();
                }
            }
            let mut here = Vec::new();
            walk(root, layers, &mut Vec::new(), &busy, &mut here);
            found.extend(here.into_iter().map(|(path, since)| ((root, path), since)));
        }
        let (time, mut ducking) = (self.time, std::mem::take(&mut self.ducking));
        for (key, busy_since) in found {
            let down = busy_since.is_some();
            let was = ducking.get(&key).map(|d| d.down);
            if was != Some(down) {
                // When the bus started, not when this step noticed it: a
                // play that began part way through a frame started the
                // ramp then, so where the level is now does not depend on
                // where the frame happened to end. Going quiet is already
                // exact, because a step always lands on a play's end.
                let since = busy_since.unwrap_or(time).min(time);
                let from = self.duck_level_at(&key, since, &ducking);
                // Where it had got to, so turning round halfway carries
                // on from there instead of jumping.
                ducking.insert(key, Ducked { down, since, from });
            }
        }
        self.ducking = ducking;
    }

    /// What is sounding on each bus right now, by the layer playing it,
    /// so a layer can be left out of its own bus.
    fn busy_buses(&self) -> BusyBuses {
        let Some(show) = &self.show else {
            return std::collections::BTreeMap::new();
        };
        let mut busy: BusyBuses = std::collections::BTreeMap::new();
        for play in &self.sounding {
            let layer = root_layers(show, play.root).and_then(|l| layer_at(l, &play.layer_path));
            let bus = match layer.map(|l| &l.kind) {
                Some(LayerKind::Audio { bus, delay, .. } | LayerKind::Video { bus, delay, .. }) => {
                    // Still waiting out its delay: not sounding yet.
                    if self.time < play.started + delay.max(0.0) {
                        continue;
                    }
                    (bus, play.started + delay.max(0.0))
                }
                _ => continue,
            };
            let (bus, since) = bus;
            busy.entry(effective_bus(bus).to_owned())
                .or_default()
                .push(((play.root, play.layer_path.clone()), since));
        }
        busy
    }

    /// Where a ducking layer's level is: 1 when its bus is quiet, the
    /// duck's `to` while it sounds, and on the ramp between.
    fn duck_level_at(
        &self,
        key: &(Root, Vec<usize>),
        time: f64,
        ducking: &HashMap<(Root, Vec<usize>), Ducked>,
    ) -> f64 {
        let Some(state) = ducking.get(key) else {
            return 1.0;
        };
        let duck = self
            .show
            .as_ref()
            .and_then(|show| root_layers(show, key.0))
            .and_then(|layers| layer_at(layers, &key.1))
            .and_then(|layer| match &layer.kind {
                LayerKind::Audio { duck, .. } | LayerKind::Video { duck, .. } => duck.as_ref(),
                _ => None,
            });
        let Some(duck) = duck else { return 1.0 };
        let (target, ramp) = if state.down {
            (duck.to, duck.attack)
        } else {
            (1.0, duck.release)
        };
        if ramp <= 0.0 || !ramp.is_finite() {
            return target;
        }
        let t = ((time - state.since) / ramp).clamp(0.0, 1.0);
        state.from + (target - state.from) * t
    }

    /// The gain multiplier a ducking layer is at now.
    fn duck_of(&self, root: Root, path: &[usize]) -> f64 {
        self.duck_level_at(&(root, path.to_vec()), self.time, &self.ducking)
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
    /// Every property every layer resolves to now, with no geometry
    /// built: the state the show is in at [`time`](Engine::time).
    ///
    /// One level below [`resolved_layers`](Engine::resolved_layers),
    /// which turns this into shapes, paths and transforms for a renderer.
    /// That step costs around ninety times what the clock does and
    /// discards nothing the timing model produced, so this is what to
    /// compare two moments by, and what a test should assert on.
    ///
    /// Layers come in draw order, each with its name, and only the
    /// properties that layer actually has.
    pub fn values(&self) -> Result<Vec<(String, Property, Value)>, Error> {
        const EVERY: [Property; 15] = [
            Property::X,
            Property::Y,
            Property::Opacity,
            Property::Scale,
            Property::ScaleX,
            Property::ScaleY,
            Property::Rotation,
            Property::Text,
            Property::Font,
            Property::Video,
            Property::Sound,
            Property::Frame,
            Property::Gain,
            Property::Visible,
            Property::Tint,
        ];
        fn walk(
            engine: &Engine,
            root: Root,
            layers: &[Layer],
            path: &mut Vec<usize>,
            out: &mut Vec<(String, Property, Value)>,
        ) {
            for (i, layer) in layers.iter().enumerate() {
                path.push(i);
                for prop in EVERY {
                    if let Some(value) = engine.resolve(root, layer, path, prop) {
                        out.push((layer.name.clone(), prop, value));
                    }
                }
                walk(engine, root, layer.children(), path, out);
                path.pop();
            }
        }
        let show = self.show.as_ref().ok_or(Error::NoShow)?;
        let mut out = Vec::new();
        walk(self, Root::Show, &show.layers, &mut Vec::new(), &mut out);
        if let Some(scene) = self.active_scene {
            if let Some(layers) = root_layers(show, Root::Scene(scene)) {
                walk(self, Root::Scene(scene), layers, &mut Vec::new(), &mut out);
            }
        }
        Ok(out)
    }

    pub fn resolved_layers(&self) -> Result<Vec<ResolvedLayer>, Error> {
        let show = self.show.as_ref().ok_or(Error::NoShow)?;
        let mut out = Vec::new();
        self.walk(
            Root::Show,
            &show.layers,
            &mut Vec::new(),
            Inherited::TOP,
            &mut out,
        )?;
        if let Some(scene) = self.active_scene {
            if let Some(layers) = root_layers(show, Root::Scene(scene)) {
                self.walk(
                    Root::Scene(scene),
                    layers,
                    &mut Vec::new(),
                    Inherited::TOP,
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
    /// resync a position that jumped, but not one merely running behind)
    /// and hosts that mix themselves read
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
                        looping,
                        delay,
                        repeat,
                        bus,
                        ..
                    } => {
                        // The duck multiplies like every other gain, so it
                        // composes with bindings and the tree above.
                        let gain = chain
                            * self.number(root, layer, path, Property::Gain).max(0.0)
                            * self.duck_of(root, path);
                        let plays = self
                            .sounding
                            .iter()
                            .filter(|s| s.root == root && s.layer_path == *path);
                        for play in plays {
                            let sound = &play.playing;
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
                                bus: Some(effective_bus(bus).to_owned()),
                            });
                        }
                    }
                    // A clip is heard when the host has registered a sound
                    // under the video's name: that is how it says this clip
                    // has a soundtrack and hands over its samples. The
                    // picture's own duration governs the position, so the
                    // two stay together through loops and repeats, and the
                    // play's id is the one `videos` reports, so a host can
                    // see that the sound and the picture are one play.
                    LayerKind::Video { bus, .. } => {
                        let Some(media) = layer.kind.media() else {
                            path.pop();
                            continue;
                        };
                        let gain = chain * self.number(root, layer, path, Property::Gain).max(0.0);
                        let plays = self
                            .sounding
                            .iter()
                            .filter(|s| s.root == root && s.layer_path == *path);
                        for play in plays {
                            let clip = &play.playing;
                            let Some(info) = self
                                .sounds
                                .get(clip)
                                .and(self.videos.get(clip))
                                .filter(|info| info.duration > 0.0)
                            else {
                                continue;
                            };
                            let elapsed = self.time - play.started - media.delay.max(0.0);
                            if elapsed < 0.0 {
                                continue;
                            }
                            let position = if media.looping || media.repeat.is_some() {
                                elapsed % info.duration
                            } else {
                                elapsed
                            };
                            out.push(Voice {
                                id: play.id,
                                layer: layer.name.clone(),
                                sound: clip.clone(),
                                position,
                                gain,
                                looping: media.looping,
                                bus: Some(effective_bus(bus).to_owned()),
                            });
                        }
                    }
                    _ => {}
                }
            }
            path.pop();
        }
    }

    /// Start every timeline whose `when` has just become true.
    ///
    /// The edge is what starts it, not the condition holding: a lamp that
    /// stays on plays its animation once. A condition that is already
    /// true when the show loads or a scene is entered counts as an edge,
    /// the same way a host firing a trigger at that moment would.
    fn follow_conditions(&mut self) {
        let Some(show) = &self.show else { return };
        // Every tree, not only the showing one: a condition in a scene
        // that is away still has to notice its variable falling, or an
        // edge that happens while it is away is invisible on return.
        let showing =
            |root: Root| root == Root::Show || Some(root) == self.active_scene.map(Root::Scene);
        let roots: Vec<Root> = std::iter::once(Root::Show)
            .chain((0..show.scenes.len()).map(Root::Scene))
            .collect();
        let mut edges: Vec<(Root, Vec<usize>, usize)> = Vec::new();
        let mut stops: Vec<(Owner, usize)> = Vec::new();
        let mut now: HashMap<(Root, Vec<usize>, usize), bool> = HashMap::new();
        for root in roots {
            let Some(layers) = root_layers(show, root) else {
                continue;
            };
            /// A timeline with a condition: where it is, and what its
            /// `when` and `while` read as now.
            type Conditioned = (Vec<usize>, usize, Option<bool>, Option<bool>);
            let mut found: Vec<Conditioned> = Vec::new();
            collect_timelines(layers, &mut Vec::new(), &mut |path, idx, tl| {
                let when = tl.when.as_ref().map(|w| self.holds(w));
                let whilst = tl.whilst.as_ref().map(|w| self.holds(w));
                if when.is_some() || whilst.is_some() {
                    found.push((path.to_vec(), idx, when, whilst));
                }
            });
            for (path, idx, when, whilst) in found {
                let key = (root, path.clone(), idx);
                if let Some(holds) = when {
                    if !showing(root) {
                        // Away: only the fall is worth remembering. Not
                        // recording the rise is what makes it an edge on
                        // return, since the value it is compared against
                        // is then the false it fell to.
                        if !holds {
                            now.insert(key, false);
                        }
                        continue;
                    }
                    // The rising edge, and only that: a condition that
                    // was already true stays quiet.
                    if holds && self.conditions.get(&key) != Some(&true) {
                        edges.push(key.clone());
                    }
                    now.insert(key, holds);
                } else if let Some(holds) = whilst {
                    if !showing(root) {
                        // A `while` is a state the scene is in, so it has
                        // nothing to remember: entering starts it again.
                        continue;
                    }
                    // No edge: it runs while it holds. Entering a scene
                    // empties the playheads, so this starts it again.
                    let running = self
                        .playing
                        .iter()
                        .any(|p| p.owner.is_layer(root, &path) && p.timeline == idx);
                    match (holds, running) {
                        (true, false) => edges.push((root, path, idx)),
                        (false, true) => stops.push((Owner::Layer { root, path }, idx)),
                        _ => {}
                    }
                }
            }
        }
        // Merged, not replaced: a scene that is not showing keeps what
        // its conditions last read, so coming back to it is not an edge
        // unless the variable turned true while it was away.
        self.conditions.extend(now);
        for (owner, timeline) in stops {
            self.playing
                .retain(|p| !(p.owner == owner && p.timeline == timeline));
        }
        for (root, path, timeline) in edges {
            self.start_timeline(root, path, timeline, self.time);
        }
    }

    /// Whether a condition reads as true right now.
    fn holds(&self, when: &crate::model::When) -> bool {
        let Some(value) = self.variables.get(&when.variable) else {
            return false;
        };
        let value = match &when.map {
            None => value.clone(),
            Some(map) => match map.get(&value.to_text()).or(when.default.as_ref()) {
                Some(mapped) => mapped.clone(),
                None => return false,
            },
        };
        match when.threshold {
            Some(level) => value.as_number() >= level,
            None => value.as_number() != 0.0,
        }
    }

    /// (Re)start the timelines `want` selects, in `root` or, with `None`,
    /// in the show's layers and the active scene.
    ///
    /// A show value's timelines are selected the same way and by the same
    /// call, since a trigger means the same thing to both. Values belong
    /// to the show, so they are left alone when only a scene is asked for.
    fn start_matching(&mut self, root: Option<Root>, at: f64, want: Want<'_>) {
        let Some(show) = &self.show else { return };
        let roots = match root {
            Some(root) => vec![root],
            None => std::iter::once(Root::Show)
                .chain(self.active_scene.map(Root::Scene))
                .collect(),
        };
        let mut starts: Vec<(Owner, usize, f64)> = Vec::new();
        for root in roots {
            let Some(layers) = root_layers(show, root) else {
                continue;
            };
            collect_timelines(layers, &mut Vec::new(), &mut |path, idx, tl| {
                if want.picks(tl.autoplay, &tl.trigger) {
                    let owner = Owner::Layer {
                        root,
                        path: path.to_vec(),
                    };
                    starts.push((owner, idx, tl.delay.max(0.0)));
                }
            });
        }
        if root.is_none_or(|root| root == Root::Show) {
            for (name, value) in &show.values {
                for (idx, tl) in value.timelines.iter().enumerate() {
                    if want.picks(tl.autoplay, &tl.trigger) {
                        starts.push((Owner::Value(name.clone()), idx, tl.delay.max(0.0)));
                    }
                }
            }
        }
        for (owner, timeline, delay) in starts {
            self.playing
                .retain(|p| !(p.owner == owner && p.timeline == timeline));
            self.playing.push(Playhead {
                owner,
                timeline,
                starts: at + delay,
                held: false,
            });
        }
    }

    /// (Re)start one timeline from the top, minding its delay.
    /// Start the timeline at `path`, as if at the instant `at`.
    ///
    /// `at` is when it should have started, not when this was noticed, so
    /// a timeline a condition starts is timed from the condition turning
    /// true.
    fn start_timeline(&mut self, root: Root, layer_path: Vec<usize>, timeline: usize, at: f64) {
        let Some(show) = &self.show else { return };
        let delay = root_layers(show, root)
            .and_then(|layers| layer_at(layers, &layer_path))
            .and_then(|l| l.timelines.get(timeline))
            .map_or(0.0, |tl| tl.delay.max(0.0));
        let owner = Owner::Layer {
            root,
            path: layer_path,
        };
        self.playing
            .retain(|p| !(p.owner == owner && p.timeline == timeline));
        self.playing.push(Playhead {
            owner,
            timeline,
            starts: at + delay,
            held: false,
        });
    }

    /// Let every binding with a debounce take in its variable: a new value
    /// becomes the candidate, and a candidate that will have held for the
    /// debounce time by `to`, where this step lands, settles, so it shows
    /// in the frame the hold runs out. A first look settles at once.
    fn settle_debounces(&mut self, to: f64) {
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
            let Some(value) = self.value(&binding.variable) else {
                debounced.remove(site);
                continue;
            };
            let settling = debounced.entry(site.clone()).or_insert_with(|| Settling {
                settled: value.clone(),
                candidate: value.clone(),
                since: self.time,
            });
            if settling.candidate != value {
                settling.candidate = value.clone();
                settling.since = self.time;
            }
            if settling.settled != settling.candidate && to + SAME_INSTANT - settling.since >= hold
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
        let spinning = std::mem::take(&mut self.spinning);
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
            // Told to spin: every cell sets off, including one already
            // standing where the row is about to land.
            let spin = spinning.contains(site);
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
                            whole: false,
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
                    whole: false,
                },
            );
            let ring_length = reel.ring();
            for (i, character) in wanted.into_iter().enumerate() {
                let Some(character) = character else { continue };
                let change = &mut records[i];
                // Where it is heading, as a place on the ring: a cell that
                // is already going there carries on, unless the row was
                // told to spin.
                if !spin && change.target.rem_euclid(ring_length) == character {
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
                    whole: false,
                };
            }
        }
        self.reels = reels;
    }

    /// Note the reel rows whose `spin` trigger is `name`; the next frame
    /// sets their cells off.
    fn set_spinning(&mut self, name: &str) {
        let Some(show) = &self.show else { return };
        let mut spinning = Vec::new();
        for site in &self.reel_sites {
            let (root, path) = site;
            if *root != Root::Show && Some(*root) != self.active_scene.map(Root::Scene) {
                continue;
            }
            let reel = root_layers(show, *root)
                .and_then(|layers| layer_at(layers, path))
                .and_then(|layer| match &layer.kind {
                    LayerKind::Digits {
                        display: DigitDisplay::Reel(reel),
                        ..
                    } => Some(reel),
                    _ => None,
                });
            if reel.is_some_and(|reel| reel.spin.contains(name)) && !self.spinning.contains(site) {
                spinning.push(site.clone());
            }
        }
        self.spinning.extend(spinning);
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
        let mut colors = std::mem::take(&mut self.color_transitions);
        for site in &self.transition_sites {
            let (root, path, index) = site;
            if *root != Root::Show && Some(*root) != self.active_scene.map(Root::Scene) {
                continue;
            }
            let layer = root_layers(show, *root).and_then(|layers| layer_at(layers, path));
            let binding = layer.and_then(|layer| layer.bindings.get(*index));
            let Some((binding, transition)) =
                binding.and_then(|b| Some((b, b.transition.as_ref()?)))
            else {
                continue;
            };
            // A modelled transition holds a filament temperature whatever
            // the property is, so it takes the numeric path.
            if binding.property == Property::Tint && transition.model.is_none() {
                let target = self
                    .binding_value(site, binding)
                    .and_then(|value| self.convert(binding, value))
                    .map(|value| value.to_text())
                    .and_then(|text| parse_color(&text));
                let Some(target) = target else {
                    colors.remove(site);
                    continue;
                };
                let change = colors.entry(site.clone()).or_insert(ColorChange {
                    start: target,
                    target,
                    started: self.time,
                });
                if change.target != target {
                    let progress = transition.value_at(0.0, 1.0, self.time - change.started);
                    *change = ColorChange {
                        start: change.value_at(progress),
                        target,
                        started: self.time,
                    };
                }
                continue;
            }
            let Some(target) = self.binding_number(site, binding) else {
                transitions.remove(site);
                continue;
            };
            let modelled = transition.model.is_some();
            let change = transitions.entry(site.clone()).or_insert(Change {
                // A show starts with its lamps cold, whatever they are
                // being told: a bulb takes its time even on the first
                // frame.
                start: if modelled {
                    crate::lamp::settled(crate::lamp::Filament::of(transition), 0.0)
                } else {
                    target
                },
                target,
                started: self.time,
                whole: target.fract() == 0.0,
            });
            if change.target != target {
                let reached = if modelled {
                    // Carry the heat over: a bulb re-lit while still warm
                    // comes up from where it is.
                    crate::lamp::temperature(
                        crate::lamp::Filament::of(transition),
                        change.start,
                        change.target,
                        self.time - change.started,
                    )
                } else {
                    transition.value_at(change.start, change.target, self.time - change.started)
                };
                *change = Change {
                    // Both values the binding was given: the one it was
                    // heading for, and the one it is heading for now.
                    whole: change.target.fract() == 0.0 && target.fract() == 0.0,
                    start: reached,
                    target,
                    started: self.time,
                };
            }
        }
        self.transitions = transitions;
        self.color_transitions = colors;
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
        if b.property == Property::Tint && transition.model.is_none() {
            let change = self.color_transitions.get(&(root, path.to_vec(), index))?;
            let progress = transition.value_at(0.0, 1.0, self.time - change.started);
            return Some(Value::Text(color_text(change.value_at(progress))));
        }
        let change = self.transitions.get(&(root, path.to_vec(), index))?;
        if let Some(crate::model::Model::Incandescent) = transition.model {
            let lamp = crate::lamp::Filament::of(transition);
            let hot = crate::lamp::temperature(
                lamp,
                change.start,
                change.target,
                self.time - change.started,
            );
            // The same filament, read two ways: how much light it gives,
            // or what colour that light is.
            return Some(match b.property {
                Property::Tint => {
                    let [r, g, bl] = crate::lamp::color(hot);
                    Value::Text(color_text([r, g, bl, 255]))
                }
                _ => Value::Number(crate::lamp::shown(lamp, hot)),
            });
        }
        let mut n = transition.value_at(change.start, change.target, self.time - change.started);
        if b.property != Property::Text {
            return Some(Value::Number(n));
        }
        // A counter between whole numbers shows whole numbers; with
        // decimals the formatting already quantises it to the last place
        // shown, so it does not flicker through digits that are rounded
        // away.
        if change.whole && b.decimals.is_none() {
            n = n.round();
        }
        Some(Value::Text(worded(b, b.format.format(n, b.decimals))))
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
        // A running timeline owns the property, over one that has
        // finished and is holding its last value.
        for running in [false, true] {
            for p in self.playing.iter().filter(|p| p.held != running) {
                if !p.owner.is_layer(root, path) {
                    continue;
                }
                let Some(tl) = layer.timelines.get(p.timeline) else {
                    continue;
                };
                // Still waiting out its delay: it owns nothing yet.
                let Some(time) = tl.local_time(p.at(self.time, tl.into())) else {
                    continue;
                };
                for track in tl.tracks.iter().filter(|t| t.property == prop) {
                    if let Some(sampled) = track.sample(time) {
                        v = Value::Number(sampled);
                    }
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
            // The show's own value when no host set one.
            _ => &self.value(&b.variable)?,
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
        // A modelled tint is fed a power level, not a colour: the model
        // decides what colour that is.
        let lamp = b.transition.as_ref().is_some_and(|t| t.model.is_some());
        let n = match (b.property, &value) {
            (Property::Tint, _) if lamp => value.as_number(),
            (Property::Font | Property::Tint, _) => return None,
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
            Property::Text => Some(Value::Text(worded(
                b,
                match value {
                    Value::Number(n) => b.format.format(scaled(b, n), b.decimals),
                    other => other.to_text(),
                },
            ))),
            Property::Visible => Some(Value::Bool(scaled(b, value.as_number()) != 0.0)),
            // Only real colors apply, as only declared styles do.
            Property::Tint => match value {
                Value::Text(color) if color.is_empty() || parse_color(&color).is_some() => {
                    Some(Value::Text(color))
                }
                _ => None,
            },
            // Any name will do: a video nobody registered simply has no
            // frames, as an unregistered image has no pixels.
            Property::Video | Property::Sound => Some(Value::Text(value.to_text())),
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
                Shape::Rect { rect, .. } => *rect,
                Shape::Circle {
                    circle: [cx, cy, r],
                } => [cx - r, cy - r, 2.0 * r, 2.0 * r],
                Shape::Path { path } => path::bounds(path.elements())?,
            },
            LayerKind::Vector { vector, size } => {
                let data = self.vectors.get(vector)?;
                let [w, h] = size.unwrap_or([data.width, data.height]);
                [0.0, 0.0, w, h]
            }
            LayerKind::Video { size, .. } => {
                // Its own size until a frame says otherwise, so a layer has
                // a box before the host has decoded anything.
                let video = &self.showing(root, path);
                let info = self.videos.get(video);
                let natural = info.map(|info| [info.width, info.height]).or_else(|| {
                    let frame = self.images.get(&frame_key(root, path))?;
                    Some([f64::from(frame.width), f64::from(frame.height)])
                })?;
                let [w, h] = size.unwrap_or(natural);
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
        self.text_raster(style_name, text, size, align, false)
            .map(TextDraw::Bitmap)
    }

    /// Rasterize (or fetch from cache) `text` in font style `style`.
    /// `None` when the style's font is not registered or nothing draws.
    ///
    /// `as_shadow` draws the same text in the style's shadow color, border
    /// included, which is the silhouette that sits behind it.
    fn text_raster(
        &self,
        style_name: &str,
        text: &str,
        size: Option<[f64; 2]>,
        align: Align,
        as_shadow: bool,
    ) -> Option<Arc<TextRaster>> {
        let style = self.show.as_ref()?.fonts.get(style_name)?;
        let registered = self.fonts.get(&style.file)?;
        let mut cache = self.text_cache.lock().unwrap_or_else(|e| e.into_inner());
        let ink = if as_shadow { "\u{1}shadow" } else { "" };
        let key = format!("{style_name}{ink}\u{1}{text}\u{1}{size:?}\u{1}{align:?}");
        if let Some(raster) = cache.rasters.get(&key) {
            return raster.clone();
        }
        let styled = cache
            .styled
            .entry(format!("{style_name}{ink}"))
            .or_insert_with(|| {
                // Colors were validated at load.
                let rgb = |c: &str| {
                    let [r, g, b, _] = parse_color(c).unwrap_or([255; 4]);
                    [r, g, b]
                };
                // A shadow is one color throughout, so its border is the
                // shadow color too.
                let color = match (as_shadow, &style.shadow) {
                    (true, Some(shadow)) => rgb(&shadow.color),
                    _ => rgb(&style.color),
                };
                let border = style
                    .border
                    .as_ref()
                    .map(|b| (if as_shadow { color } else { rgb(&b.color) }, b.width));
                Arc::new(StyledFont::new(
                    &registered.font,
                    &registered.pages,
                    color,
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
            overflow,
            transform,
        } = *placed;
        // Colors were validated at load.
        let style = self.show.as_ref().and_then(|s| s.fonts.get(style_name));
        let rgba = |c: &str| parse_color(c).unwrap_or([255; 4]);
        // The shadow goes in first, so the text lands on top of it. It is
        // the same text moved over, in one color, border included.
        let shadow = style.and_then(|s| s.shadow.as_ref()).map(|s| {
            let [sx, sy] = s.offset;
            (rgba(&s.color), sx * scale, sy * scale)
        });
        match self.text_draw(style_name, text, size, align) {
            Some(TextDraw::Bitmap(raster)) => {
                let mut bitmap = |raster: &Arc<TextRaster>, dx: f64, dy: f64, alpha: f64| {
                    let [ox, oy] = raster.offset;
                    out.push(ResolvedLayer {
                        gradient: None,
                        overflow,
                        name: name.to_owned(),
                        shape: ResolvedShape::Bitmap {
                            x: x + f64::from(ox) * scale + dx,
                            y: y + f64::from(oy) * scale + dy,
                            width: f64::from(raster.image.width) * scale,
                            height: f64::from(raster.image.height) * scale,
                            image: raster.image.clone(),
                        },
                        color: [255, 255, 255, 255],
                        opacity: opacity * alpha,
                        blend,
                        transform,
                    });
                };
                // A raster carries no color of its own to tint, so the
                // shadow is a second rasterization; its alpha rides on the
                // layer's opacity.
                if let Some(([.., a], dx, dy)) = shadow {
                    if let Some(behind) = self.text_raster(style_name, text, size, align, true) {
                        bitmap(&behind, dx, dy, f64::from(a) / 255.0);
                    }
                }
                bitmap(&raster, 0.0, 0.0, 1.0);
            }
            Some(TextDraw::Glyphs {
                font: data,
                size: em,
                glyphs,
                ..
            }) if !glyphs.is_empty() => {
                let width = style
                    .and_then(|s| s.border.as_ref())
                    .map(|b| f64::from(b.width) * scale);
                let border = style
                    .and_then(|s| s.border.as_ref())
                    .map(|b| (rgba(&b.color), f64::from(b.width) * scale));
                let mut run = |ink: [u8; 4], edge: Option<([u8; 4], f64)>, dx: f64, dy: f64| {
                    out.push(ResolvedLayer {
                        gradient: None,
                        overflow,
                        name: name.to_owned(),
                        shape: ResolvedShape::GlyphRun {
                            font: data.clone(),
                            size: em * scale,
                            glyphs: glyphs
                                .iter()
                                .map(|g| PlacedGlyph {
                                    id: g.id,
                                    x: x + g.x * scale + dx,
                                    y: y + g.y * scale + dy,
                                })
                                .collect(),
                            border: edge,
                        },
                        color: ink,
                        opacity,
                        blend,
                        transform,
                    });
                };
                if let Some((ink, dx, dy)) = shadow {
                    run(ink, width.map(|w| (ink, w)), dx, dy);
                }
                run(style.map_or([255; 4], |s| rgba(&s.color)), border, 0.0, 0.0);
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
                        gradient: None,
                        overflow: placed.overflow,
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
                    gradient: None,
                    overflow: placed.overflow,
                    name: placed.name.to_owned(),
                    shape: ResolvedShape::Image {
                        image: name.to_owned(),
                        tile: None,
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
                gradient: None,
                overflow: placed.overflow,
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

    /// Resolve `layers` under `from`, what the tree above them passes
    /// down.
    fn walk(
        &self,
        root: Root,
        layers: &[Layer],
        path: &mut Vec<usize>,
        from: Inherited,
        out: &mut Vec<ResolvedLayer>,
    ) -> Result<(), Error> {
        let Inherited {
            transform: parent,
            opacity: oa,
            overflow: bleeding,
        } = from;
        for (i, layer) in layers.iter().enumerate() {
            path.push(i);
            if self.is_visible(root, layer, path) {
                // A group that may bleed lets its whole subtree bleed.
                let overflow = bleeding || layer.overflow;
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
                                gradient: None,
                                overflow,
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
                        self.walk(
                            root,
                            children,
                            path,
                            Inherited {
                                transform: m,
                                opacity,
                                overflow,
                            },
                            out,
                        )?;
                        if clip.is_some() {
                            out.push(ResolvedLayer {
                                gradient: None,
                                overflow,
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
                                gradient: None,
                                overflow,
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
                        // A gradient keeps the layer's color as what a
                        // host without gradients would draw: its first stop.
                        let (color, gradient) = match fill {
                            crate::model::Fill::Color(color) => (
                                parse_color(color)
                                    .ok_or_else(|| Error::InvalidColor(color.clone()))?,
                                None,
                            ),
                            crate::model::Fill::Gradient(gradient) => (
                                gradient
                                    .stops()
                                    .first()
                                    .and_then(|s| parse_color(&s.color))
                                    .unwrap_or([255; 4]),
                                Some(resolve_gradient(gradient, x, y, scale)?),
                            ),
                        };
                        let stroke = stroke
                            .as_ref()
                            .map(|s| {
                                let color = parse_color(&s.color)
                                    .ok_or_else(|| Error::InvalidColor(s.color.clone()))?;
                                Ok::<_, Error>((color, s.width * scale))
                            })
                            .transpose()?;
                        out.push(ResolvedLayer {
                            gradient,
                            overflow,
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
                                    gradient: None,
                                    overflow,
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
                    LayerKind::Video { size, .. } => {
                        // The frame the host last handed over for whatever
                        // is playing. Nothing playing draws nothing, so
                        // what is behind shows through when a clip ends,
                        // and nothing is drawn before a frame arrives.
                        let playing = self
                            .sounding
                            .iter()
                            .find(|play| play.root == root && play.layer_path == *path);
                        let video = &playing.map(|play| play.playing.clone()).unwrap_or_default();
                        let key = playing.map(|_| frame_key(root, path)).unwrap_or_default();
                        if let Some(frame) = self.images.get(&key) {
                            let natural = self.videos.get(video).map_or(
                                [f64::from(frame.width), f64::from(frame.height)],
                                |info| [info.width, info.height],
                            );
                            let [width, height] = size.unwrap_or(natural);
                            out.push(ResolvedLayer {
                                gradient: None,
                                overflow,
                                name: layer.name.clone(),
                                shape: ResolvedShape::Image {
                                    image: key,
                                    tile: None,
                                    source: None,
                                    x,
                                    y,
                                    width: width * scale,
                                    height: height * scale,
                                },
                                color: [255; 4],
                                opacity,
                                blend: layer.blend,
                                transform,
                            });
                        }
                    }
                    LayerKind::Image {
                        image,
                        size,
                        sheet,
                        repeat,
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
                            // Tiling covers the layer's size with copies
                            // of one tile; without it the image is
                            // stretched to that size, as it always was.
                            let tile = repeat.map(|tile| {
                                let [tw, th] = tile.size.unwrap_or(natural);
                                Tiled {
                                    width: tw * scale,
                                    height: th * scale,
                                    offset: [
                                        self.number(root, layer, path, Property::TileX) * scale,
                                        self.number(root, layer, path, Property::TileY) * scale,
                                    ],
                                }
                            });
                            out.push(ResolvedLayer {
                                gradient: None,
                                overflow,
                                name: layer.name.clone(),
                                shape: ResolvedShape::Image {
                                    image: image.clone(),
                                    source,
                                    x,
                                    y,
                                    width: width * scale,
                                    height: height * scale,
                                    tile,
                                },
                                // Validated at load, and a binding only
                                // ever feeds it a color it could parse.
                                color: {
                                    let tint = self.text(root, layer, path, Property::Tint);
                                    parse_color(&tint).unwrap_or([255; 4])
                                },
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
                            overflow,
                            name: &layer.name,
                            origin: [x, y],
                            scale,
                            opacity,
                            blend: layer.blend,
                            transform,
                        };
                        let cells = (*digits as usize, *justify);
                        match display {
                            DigitDisplay::Segments {
                                style,
                                fill,
                                unlit,
                                slant,
                                thickness,
                                glow,
                            } => {
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
                                let look = segments::Look {
                                    thickness: thickness.unwrap_or(0.1),
                                    slant: *slant,
                                    grow: 0.0,
                                };
                                for (i, mask) in masks.into_iter().enumerate() {
                                    let cell = [x + i as f64 * cell_w, y, cell_w, height * scale];
                                    let mut push =
                                        |mask: u16, color: [u8; 4], look: segments::Look| {
                                            for points in
                                                segments::polygons(*style, mask, cell, snap, look)
                                            {
                                                out.push(ResolvedLayer {
                                                    gradient: None,
                                                    overflow,
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
                                        push(!mask, unlit, look);
                                    }
                                    // The halo first, widest and faintest
                                    // outward, so the segment itself lands
                                    // on top of it.
                                    if let Some(glow) = glow {
                                        let reach = (glow.size * cell_w).max(0.0);
                                        let strength = glow.strength.clamp(0.0, 1.0);
                                        for step in (1..=GLOW_STEPS).rev() {
                                            let part = f64::from(step) / f64::from(GLOW_STEPS);
                                            let [r, g, b, a] = lit;
                                            // Fainter the further out it
                                            // reaches, and never brighter
                                            // than the segment.
                                            let alpha = f64::from(a)
                                                * strength
                                                * (1.0 - part).max(0.0).powi(2)
                                                / f64::from(GLOW_STEPS);
                                            push(
                                                mask,
                                                [r, g, b, (alpha.clamp(0.0, 255.0)) as u8],
                                                segments::Look {
                                                    grow: reach * part * 2.0,
                                                    ..look
                                                },
                                            );
                                        }
                                    }
                                    push(mask, lit, look);
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
                            overflow,
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

/// What a layer takes from the tree above it.
#[derive(Debug, Clone, Copy)]
struct Inherited {
    transform: Transform,
    opacity: f64,
    /// Whether the tree above may draw past the canvas.
    overflow: bool,
}

impl Inherited {
    /// At the top of a tree: nothing placed, nothing faded, nothing
    /// allowed past the canvas yet.
    const TOP: Inherited = Inherited {
        transform: Transform::IDENTITY,
        opacity: 1.0,
        overflow: false,
    };
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
    /// Whether the tree this sits in may draw past the canvas.
    overflow: bool,
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

/// What a playhead has played so far.
#[derive(Debug, Clone, Copy, Default)]
struct Played {
    /// How many plays it has started, which picks from a list of assets.
    count: u64,
    /// When the last one started, which `rest` measures from.
    at: f64,
}

/// Which of `names` the play numbered `ordinal` on a layer takes.
///
/// Every mode is a function of the ordinal alone, so the same play always
/// takes the same asset: a show renders the same way twice, and a seek
/// back to a play would find what it found the first time.
fn pick_one(names: &Choice, how: Pick, ordinal: u64, seed: u64) -> String {
    let count = names.len() as u64;
    if count <= 1 {
        return names.first().to_owned();
    }
    let index = match how {
        Pick::InOrder => ordinal % count,
        Pick::Random => mix(seed ^ mix(ordinal)) % count,
        // A fresh scramble per round through the list.
        Pick::Shuffle => scramble(names.len(), seed, ordinal / count)[(ordinal % count) as usize],
    };
    names.get(index as usize).to_owned()
}

/// `0..count` in a scrambled order, the same order every time for a given
/// `seed` and `round`.
fn scramble(count: usize, seed: u64, round: u64) -> Vec<u64> {
    let mut order: Vec<u64> = (0..count as u64).collect();
    let mut state = mix(seed ^ mix(round));
    for i in (1..count).rev() {
        state = mix(state);
        order.swap(i, (state % (i as u64 + 1)) as usize);
    }
    order
}

/// Tells layers apart, so two of them picking from the same list do not
/// pick in step.
fn seed_of(root: Root, path: &[usize]) -> u64 {
    let start = match root {
        Root::Show => 0,
        Root::Scene(i) => i as u64 + 1,
    };
    path.iter()
        .fold(mix(start), |acc, i| mix(acc ^ (*i as u64 + 1)))
}

/// Scatters the bits of a counter (splitmix64's finalizer). Not random:
/// the same input always gives the same output.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
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

/// The name one video layer's picture is registered under.
///
/// A layer, not a clip: a clip playing on two layers is at two positions
/// at once, and under one name the two would share a frame. Not a name
/// a host would give an asset, since nothing else is registered with a
/// space in it.
fn frame_key(root: Root, path: &[usize]) -> String {
    let mut key = match root {
        Root::Show => "video show".to_owned(),
        Root::Scene(i) => format!("video scene {i}"),
    };
    for step in path {
        key.push_str(&format!("/{step}"));
    }
    key
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
    // Bent against the input before any change of unit, so the curve is
    // written in whatever the variable counts in.
    let n = crate::model::sample_keys(&b.curve, n).unwrap_or(n);
    n * b.scale + b.offset
}

/// A text binding's value with its words round it.
fn worded(b: &Binding, text: String) -> String {
    match (b.prefix.is_empty(), b.suffix.is_empty()) {
        (true, true) => text,
        _ => format!("{}{text}{}", b.prefix, b.suffix),
    }
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
        for color in std::iter::once(&style.color)
            .chain(style.border.as_ref().map(|b| &b.color))
            .chain(style.shadow.as_ref().map(|s| &s.color))
        {
            parse_color(color).ok_or_else(|| Error::InvalidColor(color.clone()))?;
        }
        if let Some(shadow) = &style.shadow {
            if !shadow.offset.iter().all(|n| n.is_finite()) {
                return Err(Error::InvalidShow(format!(
                    "font style {:?} needs a finite shadow offset",
                    style.file
                )));
            }
        }
    }
    fn layers(show: &Show, list: &[Layer]) -> Result<(), Error> {
        for layer in list {
            if let LayerKind::Shape {
                fill: crate::model::Fill::Gradient(gradient),
                ..
            } = &layer.kind
            {
                let stops = gradient.stops();
                let problem = if stops.is_empty() {
                    Some("needs a stop".to_owned())
                } else if !stops.iter().all(|s| s.at.is_finite()) {
                    Some("needs finite stop positions".to_owned())
                } else if stops.windows(2).any(|w| w[1].at < w[0].at) {
                    Some("needs its stops in order".to_owned())
                } else if matches!(
                    gradient,
                    crate::model::Gradient::Radial { radius, .. } if !(radius.is_finite() && *radius > 0.0)
                ) {
                    Some("needs a radius above 0".to_owned())
                } else {
                    stops
                        .iter()
                        .find(|s| parse_color(&s.color).is_none())
                        .map(|s| format!("has a stop that is not a color: {:?}", s.color))
                };
                if let Some(problem) = problem {
                    return Err(Error::InvalidShow(format!(
                        "the gradient of layer {:?} {problem}",
                        layer.name
                    )));
                }
            }
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
                if timeline.when.is_some() && timeline.whilst.is_some() {
                    return Err(Error::InvalidShow(format!(
                        "timeline {:?} of layer {:?} sets both when and while, which \
                         want different things of the same condition",
                        timeline.name, layer.name
                    )));
                }
                for condition in [&timeline.when, &timeline.whilst].into_iter().flatten() {
                    let problem = if condition.variable.is_empty() {
                        Some("needs a variable in its condition")
                    } else if condition.threshold.is_some_and(|t| !t.is_finite()) {
                        Some("needs a finite threshold in its condition")
                    } else {
                        None
                    };
                    if let Some(problem) = problem {
                        return Err(Error::InvalidShow(format!(
                            "timeline {:?} of layer {:?} {problem}",
                            timeline.name, layer.name
                        )));
                    }
                }
            }
            if let LayerKind::Audio {
                duck: Some(duck), ..
            }
            | LayerKind::Video {
                duck: Some(duck), ..
            } = &layer.kind
            {
                let finite = |n: f64| n.is_finite() && n >= 0.0;
                let problem = if duck.under.is_empty() {
                    Some("needs a bus to listen to")
                } else if !finite(duck.to) {
                    Some("needs a gain of 0 or more to duck to")
                } else if !finite(duck.attack) || !finite(duck.release) {
                    Some("needs an attack and release of 0 or more")
                } else {
                    None
                };
                if let Some(problem) = problem {
                    return Err(Error::InvalidShow(format!(
                        "the duck of layer {:?} {problem}",
                        layer.name
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
                if binding.decimals.is_some_and(|d| d > 15) {
                    return Err(Error::InvalidShow(format!(
                        "the {:?} binding of layer {:?} asks for more decimals than a number has",
                        binding.property, layer.name
                    )));
                }
                if binding.debounce.is_some_and(|d| !d.is_finite() || d < 0.0) {
                    return Err(Error::InvalidShow(format!(
                        "the {:?} binding of layer {:?} needs a debounce of 0 or more",
                        binding.property, layer.name
                    )));
                }
                if !binding.curve.is_empty() {
                    let sorted = binding.curve.windows(2).all(|w| w[0].t <= w[1].t);
                    let finite = binding
                        .curve
                        .iter()
                        .all(|k| k.t.is_finite() && k.v.is_finite());
                    let problem = if binding.threshold.is_some() {
                        // The same job: a threshold is a curve of two keys
                        // with a `step` ease, so doing both says nothing
                        // clear about which happens first.
                        Some("sets both curve and threshold, which are the same job")
                    } else if !finite {
                        Some("needs finite curve keys")
                    } else if !sorted {
                        Some("needs its curve keys in order of input")
                    } else {
                        None
                    };
                    if let Some(problem) = problem {
                        return Err(Error::InvalidShow(format!(
                            "the {:?} binding of layer {:?} {problem}",
                            binding.property, layer.name
                        )));
                    }
                }
                if let Some(transition) = &binding.transition {
                    let positive = |n: f64| n.is_finite() && n > 0.0;
                    let ring = transition.wrap.is_some() || transition.direction.is_some();
                    let modelled = transition.model.is_some();
                    let problem = if matches!(binding.property, Property::Font | Property::Visible)
                    {
                        Some("is on a binding that cannot be eased")
                    } else if binding.property == Property::Tint && ring {
                        Some("sets wrap or direction, which a color has no use for")
                    } else if modelled
                        && (positive(transition.duration)
                            || ring
                            || transition.step.is_some()
                            || !transition.offset.is_empty()
                            || transition.ease != crate::easing::Easing::default())
                    {
                        Some("follows a model, which decides its own timing, shape and way round")
                    } else if !modelled
                        && (transition.kelvin.is_some()
                            || transition.heating.is_some()
                            || transition.cooling.is_some())
                    {
                        Some("shapes a filament without naming a model to follow")
                    } else if modelled
                        && [transition.kelvin, transition.heating, transition.cooling]
                            .into_iter()
                            .flatten()
                            .any(|n| !positive(n))
                    {
                        Some("needs a kelvin, heating and cooling above 0")
                    } else if !modelled && !positive(transition.duration) {
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
                // A modelled tint takes a power level, not a colour: the
                // filament decides what colour that is.
                let lamp = binding
                    .transition
                    .as_ref()
                    .is_some_and(|t| t.model.is_some());
                if binding.property == Property::Tint && !lamp {
                    let mapped = binding.map.iter().flat_map(|m| m.values());
                    for value in mapped.chain(&binding.default) {
                        let color = matches!(value, Value::Text(c) if c.is_empty()
                            || parse_color(c).is_some());
                        if !color {
                            return Err(Error::InvalidShow(format!(
                                "tint binding of layer {:?} maps to {value:?}, not a color",
                                layer.name
                            )));
                        }
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
            if let Some(media) = layer.kind.media() {
                let gain = match &layer.kind {
                    LayerKind::Audio { gain, .. } | LayerKind::Video { gain, .. } => *gain,
                    _ => 1.0,
                };
                let problem = if media.looping && media.repeat.is_some() {
                    Some("sets both loop and repeat")
                } else if !media.delay.is_finite() || media.delay < 0.0 {
                    Some("needs a delay of 0 or more")
                } else if media.repeat.is_some_and(|r| !r.is_finite() || r < 0.0) {
                    Some("needs a repeat of 0 or more")
                } else if !gain.is_finite() || gain < 0.0 {
                    Some("needs a gain of 0 or more")
                } else if media.retrigger == Retrigger::Overlap && media.voices == 0 {
                    Some("needs at least one voice to overlap")
                } else if media.retrigger == Retrigger::Overlap && media.kind == MediaKind::Video {
                    Some("cannot overlap: a video layer shows one picture at a time")
                } else if media.names.is_empty() {
                    Some("names nothing to play")
                } else if !media.rest.is_finite() || media.rest < 0.0 {
                    Some("needs a rest of 0 or more")
                } else {
                    None
                };
                if let Some(problem) = problem {
                    let kind = match media.kind {
                        MediaKind::Sound => "audio",
                        MediaKind::Video => "video",
                    };
                    return Err(Error::InvalidShow(format!(
                        "{kind} layer {:?} {problem}",
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

/// Warn about bindings that will quietly do nothing.
///
/// A bound value the engine cannot use leaves the property as it was,
/// silently, the way an unregistered image simply does not draw: the host
/// may send something usable later, and a frame is no place to complain.
/// That is right at runtime and useless while writing a show, where a
/// mistyped variable or a color that is not one looks exactly like a
/// feature that does not work.
///
/// What a show does state up front is which variables it declares and
/// what they start at, so that is what is checked. Values written in the
/// show itself, like the colors and styles a `map` lists, are errors at
/// load instead.
fn quiet_bindings(show: &Show, out: &mut Vec<String>) {
    fn walk(show: &Show, layers: &[Layer], out: &mut Vec<String>) {
        for layer in layers {
            for binding in &layer.bindings {
                // A curve bends a number, and these properties never hold
                // one, so it would quietly do nothing.
                if !binding.curve.is_empty()
                    && matches!(
                        binding.property,
                        Property::Tint | Property::Font | Property::Video | Property::Sound
                    )
                {
                    out.push(format!(
                        "the {:?} binding of layer {:?} has a curve, which only bends a number; \
                         this property never holds one",
                        binding.property, layer.name
                    ));
                }
                // Words only go round text; every other property holds
                // a number or a name of its own.
                if (!binding.prefix.is_empty() || !binding.suffix.is_empty())
                    && binding.property != Property::Text
                {
                    out.push(format!(
                        "the {:?} binding of layer {:?} has words round its value, which only a \
                         text binding shows",
                        binding.property, layer.name
                    ));
                }
                let name = &binding.variable;
                if !show.variables.contains_key(name) && show.values.contains_key(name) {
                    // A value the show animates is declared as much as a
                    // variable is. It is always a number, though, so a
                    // property that cannot take one without a map still
                    // gets nothing.
                    let problem = match binding.property {
                        _ if binding.map.is_some() => None,
                        Property::Tint => Some("a color like \"#RRGGBB\""),
                        Property::Font => Some("one of the show's font styles"),
                        _ => None,
                    };
                    if let Some(wanted) = problem {
                        out.push(format!(
                            "the {:?} binding of layer {:?} reads value {name:?}, which is a \
                             number, not {wanted}; values it cannot use leave the property alone",
                            binding.property, layer.name
                        ));
                    }
                    continue;
                }
                let Some(value) = show.variables.get(name) else {
                    out.push(format!(
                        "the {:?} binding of layer {:?} reads variable {name:?}, which the show \
                         does not declare; it does nothing until a host sets that variable",
                        binding.property, layer.name
                    ));
                    continue;
                };
                // With a map it is the mapped values that reach the
                // property, and those are checked at load.
                if binding.map.is_some() {
                    continue;
                }
                // A modelled tint reads a power level, not a colour.
                if binding
                    .transition
                    .as_ref()
                    .is_some_and(|t| t.model.is_some())
                {
                    continue;
                }
                let text = value.to_text();
                let problem = match binding.property {
                    Property::Tint if !text.is_empty() && parse_color(&text).is_none() => {
                        Some("a color like \"#RRGGBB\"")
                    }
                    Property::Font if !show.fonts.contains_key(&text) => {
                        Some("one of the show's font styles")
                    }
                    _ => None,
                };
                if let Some(wanted) = problem {
                    out.push(format!(
                        "the {:?} binding of layer {:?} reads variable {name:?}, which starts at \
                         {text:?}, not {wanted}; values it cannot use leave the property alone",
                        binding.property, layer.name
                    ));
                }
            }
            walk(show, layer.children(), out);
        }
    }
    for layers in show.layer_trees() {
        walk(show, layers, out);
    }
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

/// The outline of a rect, with rounded corners when it has a radius.
///
/// Quarter circles as cubics, the same approximation SVG arcs get, so a
/// rounded rect strokes and clips like any other path.
fn rect_path(rect: [f64; 4], radius: Option<f64>) -> Vec<PathElement> {
    let [x, y, w, h] = rect;
    let r = Shape::corner_radius(rect, radius);
    if r <= 0.0 {
        return vec![
            PathElement::MoveTo([x, y]),
            PathElement::LineTo([x + w, y]),
            PathElement::LineTo([x + w, y + h]),
            PathElement::LineTo([x, y + h]),
            PathElement::Close,
        ];
    }
    // How far a cubic's control point sits along the tangent to meet a
    // quarter circle: the usual 4/3 * (sqrt(2) - 1).
    let k = r * 0.552_284_749_830_793_4;
    let (r1, b) = (x + w, y + h);
    vec![
        PathElement::MoveTo([x + r, y]),
        PathElement::LineTo([r1 - r, y]),
        PathElement::CubicTo([r1 - r + k, y], [r1, y + r - k], [r1, y + r]),
        PathElement::LineTo([r1, b - r]),
        PathElement::CubicTo([r1, b - r + k], [r1 - r + k, b], [r1 - r, b]),
        PathElement::LineTo([x + r, b]),
        PathElement::CubicTo([x + r - k, b], [x, b - r + k], [x, b - r]),
        PathElement::LineTo([x, y + r]),
        PathElement::CubicTo([x, y + r - k], [x + r - k, y], [x + r, y]),
        PathElement::Close,
    ]
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
        (
            Shape::Rect {
                rect: [rx, ry, w, h],
                radius,
            },
            None,
        ) if Shape::corner_radius([*rx, *ry, *w, *h], *radius) <= 0.0 => ResolvedShape::Rect {
            x: rx * scale + x,
            y: ry * scale + y,
            width: w * scale,
            height: h * scale,
        },
        (
            Shape::Circle {
                circle: [cx, cy, r],
            },
            None,
        ) => ResolvedShape::Circle {
            cx: cx * scale + x,
            cy: cy * scale + y,
            radius: r * scale,
        },
        (
            Shape::Rect {
                rect: [rx, ry, w, h],
                radius,
            },
            stroke,
        ) => ResolvedShape::Path {
            elements: rect_path([*rx, *ry, *w, *h], *radius)
                .into_iter()
                .map(|e| e.map(place))
                .collect(),
            stroke,
        },
        (
            Shape::Circle {
                circle: [cx, cy, r],
            },
            stroke,
        ) => {
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
        (Shape::Path { path: data }, stroke) => ResolvedShape::Path {
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
    /// Fill color as RGBA bytes; opaque white for images. With a
    /// `gradient` this is its first stop, so a host that draws no
    /// gradients still draws something sensible.
    pub color: [u8; 4],
    /// A gradient to fill the shape with instead of `color`, already in
    /// the same space as the shape's coordinates.
    pub gradient: Option<ResolvedGradient>,
    /// Effective opacity in [0, 1] (tree-multiplied).
    pub opacity: f64,
    /// How the item combines with what was painted before it. Markers
    /// (clips, blend groups) carry `Normal`.
    pub blend: Blend,
    /// Whether this item may draw past the canvas into the letterbox; see
    /// [`Layer::overflow`](crate::Layer::overflow). A host that fits the
    /// canvas into a larger surface leaves these unclipped.
    pub overflow: bool,
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
        /// Repeat one tile across the box instead of stretching the image
        /// to fill it.
        tile: Option<Tiled>,
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

/// A gradient with its colors parsed and its geometry scaled, ready to
/// draw: the shape's own space, like the shape's coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedGradient {
    pub kind: ResolvedGradientKind,
    /// `(position, RGBA)`, in order, at least one.
    pub stops: Vec<(f32, [u8; 4])>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResolvedGradientKind {
    Linear { from: [f64; 2], to: [f64; 2] },
    Radial { center: [f64; 2], radius: f64 },
}

/// Parse a gradient's colors and scale its geometry the way a shape's
/// coordinates are scaled.
fn resolve_gradient(
    gradient: &crate::model::Gradient,
    x: f64,
    y: f64,
    scale: f64,
) -> Result<ResolvedGradient, Error> {
    use crate::model::Gradient;
    let stops = gradient
        .stops()
        .iter()
        .map(|stop| {
            parse_color(&stop.color)
                .map(|rgba| (stop.at as f32, rgba))
                .ok_or_else(|| Error::InvalidColor(stop.color.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let place = |[px, py]: [f64; 2]| [px * scale + x, py * scale + y];
    let kind = match gradient {
        Gradient::Linear { from, to, .. } => ResolvedGradientKind::Linear {
            from: place(*from),
            to: place(*to),
        },
        Gradient::Radial { center, radius, .. } => ResolvedGradientKind::Radial {
            center: place(*center),
            radius: radius * scale,
        },
    };
    Ok(ResolvedGradient { kind, stops })
}

/// The bus a layer's sound is on: the one it names, or [`MAIN_BUS`].
fn effective_bus(bus: &Option<String>) -> &str {
    bus.as_deref().unwrap_or(crate::model::MAIN_BUS)
}

/// How a tiled image covers its box: one tile's size and where the
/// pattern starts, both in the same units as the box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tiled {
    pub width: f64,
    pub height: f64,
    pub offset: [f64; 2],
}
