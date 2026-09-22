use crate::easing::Easing;
use crate::path::PathData;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The show format version this engine reads and writes. It goes up when a
/// change could make an older engine misread a show; engines refuse shows
/// of a newer format instead of playing them wrongly.
pub const FORMAT: u32 = 1;

fn default_format() -> u32 {
    1
}

/// A declarative show description: what a `.json` show file deserializes into.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Show {
    /// Show format version, see [`FORMAT`]. A show without it is format 1.
    #[serde(default = "default_format")]
    pub format: u32,
    pub name: String,
    /// Logical canvas size in pixels `[width, height]`. Hosts scale the
    /// rendered texture; content is authored against this space.
    /// Coordinates have their origin at the top-left corner: x grows
    /// right, y grows down.
    pub size: [u32; 2],
    /// Background color, `#RRGGBB` or `#RRGGBBAA`.
    #[serde(default = "default_background")]
    pub background: String,
    /// How rendered colors reach the display: full color by default, or
    /// quantized luminance tinted in one color (DMD style). Scenes can
    /// override it.
    #[serde(default)]
    pub output: Output,
    /// Named text styles for text layers: a bitmap font plus colors.
    #[serde(default)]
    pub fonts: BTreeMap<String, FontStyle>,
    /// Declared variables and their initial values.
    #[serde(default)]
    pub variables: BTreeMap<String, Value>,
    /// Layers that are always present, painted behind the active scene.
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// Switchable views; exactly one is active at a time (the first one
    /// when the show loads), entered by firing its trigger.
    #[serde(default)]
    pub scenes: Vec<Scene>,
}

/// The trigger names something listens to: in a show document one name
/// (`"go"`), a list (`["turn_left", "hazard"]`), or nothing. Firing any of
/// them has the same effect.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Triggers(pub Vec<String>);

impl Triggers {
    pub fn contains(&self, name: &str) -> bool {
        self.0.iter().any(|t| t == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Triggers {
    /// Written back the way it is usually authored: nothing, one name, or
    /// a list.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [] => serializer.serialize_none(),
            [one] => serializer.serialize_str(one),
            many => many.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Triggers {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Form {
            One(String),
            Many(Vec<String>),
        }
        Ok(Triggers(match Option::<Form>::deserialize(deserializer)? {
            None => Vec::new(),
            Some(Form::One(name)) => vec![name],
            Some(Form::Many(names)) => names,
        }))
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Triggers {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Triggers".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A trigger name, or a list of names; firing any of them has the same effect.",
            "anyOf": [
                { "type": "string" },
                { "type": "array", "items": { "type": "string" } },
                { "type": "null" }
            ]
        })
    }
}

/// A switchable view of the show: its layers render only while it is the
/// active scene. Entering a scene (again) restarts it: timelines of the
/// previous scene stop, the entered scene's autoplay timelines start at 0.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Scene {
    pub name: String,
    /// Trigger name, or list of names, that enters this scene.
    #[serde(default)]
    pub trigger: Triggers,
    /// Output color handling while this scene is active; the show's when
    /// omitted.
    #[serde(default)]
    pub output: Option<Output>,
    #[serde(default)]
    pub layers: Vec<Layer>,
}

/// How the finished frame reaches the display. Every field is optional: a
/// scene's output overrides only the fields it sets, the rest come from
/// the show's, then from the defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Output {
    /// Color conversion; `rgb` by default.
    #[serde(default)]
    pub mode: Option<OutputMode>,
    /// Color that full luminance maps to in the gray modes, `#RRGGBB`
    /// (white by default). Ignored in `rgb` mode.
    #[serde(default)]
    pub tint: Option<String>,
    /// How hosts scale the frame up to their surface; `smooth` by default.
    #[serde(default)]
    pub scaling: Option<Scaling>,
    /// Effects applied to the finished frame as it is shown, in order. A
    /// scene's list replaces the show's; an empty list turns them off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passes: Option<Vec<Pass>>,
}

/// An effect on the finished frame, applied where it is shown (windows, the
/// web player), not by the offscreen renderer: it works at the surface's
/// resolution, which a frame at canvas size does not have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Pass {
    /// Every canvas pixel becomes a dot, as on a dot matrix display.
    Dots(Dots),
}

/// The dot matrix look: canvas pixels shown as separate dots on black.
/// Below three surface pixels per dot there is no room for it and the frame
/// is shown plain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Dots {
    /// Dot diameter as a share of the pixel pitch, above 0 up to 1.
    #[serde(default = "default_dot_size")]
    pub size: f64,
    #[serde(default)]
    pub shape: DotShape,
    /// Color of a dot that is off, `#RRGGBB`: the faint dots of a real
    /// panel. Dots never get darker than this. None by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlit: Option<String>,
    /// How much lit dots bleed into the dark around them, 0 (not at all,
    /// the default) to 1.
    #[serde(default)]
    pub glow: f64,
}

fn default_dot_size() -> f64 {
    0.8
}

impl Default for Dots {
    fn default() -> Self {
        Self {
            size: default_dot_size(),
            shape: DotShape::default(),
            unlit: None,
            glow: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum DotShape {
    #[default]
    Round,
    /// Square dots with gaps: an LED matrix.
    Square,
}

impl Output {
    /// This output with its unset fields taken from `base`.
    pub fn over(&self, base: &Output) -> Output {
        Output {
            mode: self.mode.or(base.mode),
            tint: self.tint.clone().or_else(|| base.tint.clone()),
            scaling: self.scaling.or(base.scaling),
            passes: self.passes.clone().or_else(|| base.passes.clone()),
        }
    }
}

/// How hosts should scale the rendered frame up to their surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Scaling {
    /// Any factor, smoothly filtered.
    #[default]
    Smooth,
    /// Whole-number factors with nearest-neighbor sampling, so every
    /// canvas pixel becomes a crisp square block (DMD-resolution content).
    PixelPerfect,
}

/// How the finished frame's colors are converted for the display.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum OutputMode {
    /// Full color, unchanged.
    #[default]
    Rgb,
    /// Luminance quantized to 4 levels (2 bits), times the tint.
    Gray2,
    /// Luminance quantized to 16 levels (4 bits), times the tint.
    Gray4,
}

/// A text style: a bitmap font the host registered, tinted and optionally
/// outlined.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FontStyle {
    /// Name of the font as registered by the host, by convention the font
    /// file's stem: a bitmap font ([`Engine::set_font`](crate::Engine::set_font))
    /// or an outline font (`Engine::set_outline_font`). Which kind it is
    /// decides how the text is drawn; the layers using the style do not
    /// change.
    pub file: String,
    /// Em size in canvas pixels. Required for outline fonts; not allowed
    /// for bitmap fonts, which have one fixed size.
    #[serde(default)]
    pub size: Option<f64>,
    /// Text color, `#RRGGBB`. For bitmap fonts it multiplies the glyph
    /// colors, so white keeps the font's own.
    #[serde(default = "default_font_color")]
    pub color: String,
    #[serde(default)]
    pub border: Option<Border>,
}

/// A border of `width` pixels drawn outside every glyph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Border {
    /// `#RRGGBB`
    pub color: String,
    /// Outline width in pixels.
    #[serde(default = "default_border_width")]
    pub width: u32,
}

fn default_font_color() -> String {
    "#FFFFFF".to_owned()
}

fn default_border_width() -> u32 {
    1
}

/// Where content sits in a box: one of nine positions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Align {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Align {
    /// Offset that places a `width` x `height` item in a container.
    pub fn offset(self, width: f64, height: f64, container_w: f64, container_h: f64) -> (f64, f64) {
        use Align::*;
        let x = match self {
            TopLeft | Left | BottomLeft => 0.0,
            Top | Center | Bottom => (container_w - width) / 2.0,
            TopRight | Right | BottomRight => container_w - width,
        };
        let y = match self {
            TopLeft | Top | TopRight => 0.0,
            Left | Center | Right => (container_h - height) / 2.0,
            BottomLeft | Bottom | BottomRight => container_h - height,
        };
        (x, y)
    }

    pub(crate) fn is_left(self) -> bool {
        matches!(self, Align::TopLeft | Align::Left | Align::BottomLeft)
    }
}

fn default_background() -> String {
    "#000000".to_owned()
}

/// One node in the show tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Layer {
    pub name: String,
    #[serde(flatten)]
    pub kind: LayerKind,
    /// Translation applied to this layer (and its subtree, for groups),
    /// in canvas coordinates (origin top-left, y down).
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    /// Opacity in [0, 1], multiplied down the tree.
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    /// Uniform scale of the layer around its x/y origin (shape coordinates
    /// and sizes, image destination size), or around its anchor point.
    /// A group's scale applies to its whole subtree.
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// Scale along x and y on top of `scale`, for stretching and flips
    /// (negative values mirror). Inherited like `scale`.
    #[serde(default = "default_scale")]
    pub scale_x: f64,
    #[serde(default = "default_scale")]
    pub scale_y: f64,
    /// Rotation in degrees, clockwise on the canvas, around the layer's
    /// x/y origin or its anchor point. A group turns its whole subtree.
    #[serde(default)]
    pub rotation: f64,
    /// Which point of the layer's content box sits at its x/y. Without an
    /// anchor, images put their top-left corner there and shapes their
    /// local origin. Not supported on groups.
    #[serde(default)]
    pub anchor: Option<Align>,
    #[serde(default = "default_visible")]
    pub visible: bool,
    /// How the layer combines with what is painted beneath it; a group
    /// blends its children as one picture.
    #[serde(default)]
    pub blend: Blend,
    /// Live property bindings: `property = variable * scale + offset`.
    #[serde(default)]
    pub bindings: Vec<Binding>,
    /// Keyframed animations on this layer's properties.
    #[serde(default)]
    pub timelines: Vec<Timeline>,
}

fn default_opacity() -> f64 {
    1.0
}

/// How a layer's colors combine with the colors already beneath it.
/// Opacity applies on top of the blend, as usual.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Blend {
    /// Paint over: the layer covers what is beneath.
    #[default]
    Normal,
    /// Sum the colors: light that adds to the picture, as a lamp behind
    /// art does. Overlapping glows brighten each other; white saturates.
    Add,
    /// `1 - (1 - a)(1 - b)`: light that adds but never saturates, softer
    /// than `add`.
    Screen,
    /// Product of the colors: a coloured shape darkens and tints what is
    /// beneath, as a gel over a lamp does; white leaves it alone.
    Multiply,
}

fn default_visible() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum LayerKind {
    /// A container: children are positioned relative to the group and
    /// painted in order. With `clip`, children only show inside that
    /// shape, given in the group's local space like a shape layer's.
    Group {
        children: Vec<Layer>,
        #[serde(default)]
        clip: Option<Shape>,
        /// Loudness of the sounds in the subtree, multiplied down the tree
        /// like opacity.
        #[serde(default = "default_scale")]
        gain: f64,
    },
    /// A vector shape filled with `fill`, and outlined by `stroke` when
    /// given.
    Shape {
        shape: Shape,
        fill: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Stroke>,
    },
    /// A host-provided raster image, registered under `image` via
    /// [`Engine::set_image`](crate::Engine::set_image). Drawn with its
    /// top-left corner at the layer's x/y unless the layer has an
    /// `anchor`; not yet registered images are skipped.
    Image {
        image: String,
        /// Destination size `[width, height]`; the image's natural size
        /// (one cell's size with a `sheet`) when omitted.
        #[serde(default)]
        size: Option<[f64; 2]>,
        /// Treat the image as a grid of equally sized cells and draw one:
        /// the one the `frame` property selects.
        #[serde(default)]
        sheet: Option<Sheet>,
        /// Base cell index for sheets (row-major, 0 is the top-left cell).
        #[serde(default)]
        frame: f64,
        /// Color the image is multiplied by, `#RRGGBB` or `#RRGGBBAA`:
        /// white leaves it alone, a color stains it (a lamp behind white
        /// art, a worn look, one sprite in several colors).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tint: Option<String>,
    },
    /// Text in a bitmap font style from the show's `fonts`. With `size` the
    /// text is aligned inside that box (its top-left corner at the layer's
    /// x/y); without, the box is the text's own size. Multi-line text
    /// (`\n`) aligns each line on its own. Skipped while the style's font
    /// is not registered.
    Text {
        text: String,
        font: String,
        #[serde(default)]
        size: Option<[f64; 2]>,
        #[serde(default)]
        align: Align,
    },
    /// Vector artwork the host registered under `vector`
    /// ([`Engine::set_vector`](crate::Engine::set_vector), the loader does
    /// it for `assets/*.svg`), drawn like an image: its top-left corner at
    /// the layer's x/y (or by `anchor`), at its natural size or scaled
    /// into `size`. Skipped while not registered.
    Vector {
        vector: String,
        /// Destination size `[width, height]`; the artwork's own size
        /// when omitted.
        #[serde(default)]
        size: Option<[f64; 2]>,
    },
    /// A row of `digits` equal cells across `size` `[width, height]`
    /// (top-left at the layer's x/y) showing `text`, one character per
    /// cell; how a cell is drawn is up to `display`. Text longer than the
    /// row is cut at the far side of `justify`.
    Digits {
        digits: u32,
        size: [f64; 2],
        #[serde(default)]
        text: String,
        #[serde(default)]
        justify: Justify,
        display: DigitDisplay,
    },
    /// A sound the host registered under `sound` with its duration
    /// ([`Engine::set_sound`](crate::Engine::set_sound)), played the way a
    /// timeline is: on `trigger` or at load with `autoplay`, after `delay`,
    /// looping or repeating, firing `on_end`. Draws nothing. What plays is
    /// reported by [`Engine::voices`](crate::Engine::voices); the engine
    /// never touches samples.
    Audio {
        sound: String,
        /// Trigger name, or list of names, that plays it.
        #[serde(default)]
        trigger: Triggers,
        /// Play when the show loads or the scene is entered.
        #[serde(default)]
        autoplay: bool,
        /// Repeat forever. Cannot be combined with `repeat`.
        #[serde(default, rename = "loop")]
        looping: bool,
        /// Seconds between the trigger and the first sample.
        #[serde(default)]
        delay: f64,
        /// Number of plays (fractions allowed); once when omitted.
        #[serde(default)]
        repeat: Option<f64>,
        /// Trigger fired when a play finishes (never for loops).
        #[serde(default)]
        on_end: Option<String>,
        /// Trigger name, or list of names, that stops it.
        #[serde(default)]
        stop: Triggers,
        /// What the trigger does while the sound is already playing.
        #[serde(default)]
        retrigger: Retrigger,
        /// With `overlap`, how many plays may sound at once; the oldest
        /// stops beyond it. Default 4.
        #[serde(default = "default_voices")]
        voices: u32,
        /// Loudness, 0 to 1 and above, times the gains of the groups above
        /// it. A normal numeric property: bindable and animatable.
        #[serde(default = "default_scale")]
        gain: f64,
        /// Name of the bus the sound plays through; hosts route buses to
        /// outputs. Their default when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bus: Option<String>,
    },
}

fn default_voices() -> u32 {
    4
}

/// What an audio layer's trigger does while the layer is already playing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Retrigger {
    /// Start over: the play so far stops.
    #[default]
    Restart,
    /// Start another play on top, up to the layer's `voices`.
    Overlap,
    /// Let the play finish; the trigger does nothing.
    Ignore,
}

/// A sprite sheet layout: cells of `cell` `[width, height]` pixels,
/// `columns` per row, numbered row by row from the top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Sheet {
    pub cell: [u32; 2],
    pub columns: u32,
}

/// Which end of a digit row its text sits against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Justify {
    #[default]
    Left,
    /// As scores are shown: the last character in the last cell.
    Right,
}

/// Characters a reel carries when its show does not say.
fn default_charset() -> String {
    "0123456789".to_owned()
}

/// One character at a time, the way a wheel that lands on every character
/// travels.
fn default_reel_step() -> Option<f64> {
    Some(1.0)
}

fn default_window() -> u32 {
    1
}

/// A row of cells carrying a ring of symbols, each rolling to the symbol
/// the layer's `text` asks of it, as the wheels of an odometer, a counter
/// or a departure board do. Each cell stands somewhere on the ring of its
/// own, so a change moves only the cells it reaches, and `stagger` keeps
/// them from moving in lockstep.
///
/// A symbol is named by a character of `charset` and drawn either as that
/// character in a font style of the show, which needs no artwork and
/// stays sharp at any size, or as the artwork `cells` gives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Reel {
    /// The characters naming the ring's symbols, in the order they pass
    /// by. A cell shows nothing for anything the text asks of it that the
    /// ring does not carry.
    #[serde(default = "default_charset")]
    pub charset: String,
    /// Font style from the show's `fonts` the symbols are drawn in as
    /// their own characters, when `cells` does not put artwork on the ring
    /// instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    /// What each symbol looks like, one entry per character of `charset`.
    /// Without it a symbol is drawn as its own character, in `font`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<ReelCells>,
    /// Seconds one move takes; above 0.
    pub duration: f64,
    /// How a step progresses; the same easings timelines know.
    #[serde(default)]
    pub ease: Easing,
    /// Which way round the ring a cell travels: `shortest` by default,
    /// `forward` for a wheel that only turns one way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// How far a cell travels in one move, in symbols: one by default, so
    /// it lands on every symbol on the way, as a counter does. `null`
    /// makes the whole journey one move instead, which is how a wheel
    /// spins: `duration` then covers all of it and `ease` shapes the spin
    /// rather than each symbol.
    /// Always written back, `null` included: absent means one symbol at a
    /// time, which is not what `null` means, so dropping it would change
    /// a spinning wheel into a stepping one.
    #[serde(default = "default_reel_step")]
    pub step: Option<f64>,
    /// Extra whole turns of the ring a cell makes before it lands, on top
    /// of the distance to its symbol. 0 by default; a spinning wheel takes
    /// a few.
    #[serde(default)]
    pub turns: u32,
    /// How many symbols of the ring the cell shows at once, stacked with
    /// the one it stands on in the middle. 1 by default; a wheel behind a
    /// tall window shows its neighbours as well.
    #[serde(default = "default_window")]
    pub window: u32,
    /// Motion added on top of a move, in symbols: keys like a binding
    /// transition's `offset`, so a cell can settle against its stop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offset: Vec<Key>,
    /// Seconds each cell waits behind the one to its right, so a row does
    /// not move as one piece. 0 by default.
    #[serde(default)]
    pub stagger: f64,
    /// Trigger name, or list of names, that sets the row spinning: every
    /// cell travels its `turns` and lands on the symbol its text names at
    /// that moment, whether or not that is the one it already shows.
    #[serde(default)]
    pub spin: Triggers,
}

/// What a reel's symbols are drawn as. The charset stays the ring's
/// identity, so a show still says which symbol a cell lands on by its
/// character; this only says what that symbol looks like.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum ReelCells {
    /// Vector artwork the host registered, by name, one per symbol.
    /// Artwork scales with the row, so a reel of pictures is as sharp as
    /// one of letters.
    Vectors(Vec<String>),
    /// Images the host registered, by name, one per symbol.
    Images(Vec<String>),
}

impl ReelCells {
    /// How many symbols the artwork covers.
    pub fn len(&self) -> usize {
        match self {
            ReelCells::Vectors(names) | ReelCells::Images(names) => names.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The asset drawn for symbol `index` of the ring.
    pub fn at(&self, index: usize) -> Option<&str> {
        match self {
            ReelCells::Vectors(names) | ReelCells::Images(names) => {
                names.get(index).map(String::as_str)
            }
        }
    }
}

impl Reel {
    /// The characters naming the ring's symbols, in order.
    pub fn characters(&self) -> Vec<char> {
        self.charset.chars().collect()
    }

    /// How many symbols the ring holds.
    pub fn ring(&self) -> f64 {
        self.charset.chars().count().max(1) as f64
    }

    /// How a cell travels from one symbol to another. Symbols are counted
    /// straight, not around the ring, so a journey can be longer than one
    /// turn; where a cell stands is that count folded back onto the ring.
    pub fn roll(&self) -> Transition {
        Transition {
            duration: self.duration,
            ease: self.ease,
            wrap: None,
            direction: None,
            step: self.step,
            offset: self.offset.clone(),
        }
    }

    /// Where a cell standing at `from` travels to show the symbol that
    /// `character` names: the way round `direction` asks for, plus the
    /// turns it takes before landing.
    pub fn travel(&self, from: f64, character: f64) -> f64 {
        let ring = self.ring();
        let forward = (character - from).rem_euclid(ring);
        let step = match self.direction.unwrap_or_default() {
            Direction::Backward => forward - ring,
            Direction::Shortest if forward > ring / 2.0 => forward - ring,
            _ => forward,
        };
        // Turns go the way the cell is already travelling.
        let turns = f64::from(self.turns) * ring;
        from + step + if step < 0.0 { -turns } else { turns }
    }
}

/// How the cells of a digit row are drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum DigitDisplay {
    /// A segment display: lit segments in `fill`, and the dark ones in
    /// `unlit` when given. A `.` or `,` lights the dot of the cell before
    /// it instead of taking a cell. Characters the style cannot show stay
    /// dark.
    Segments {
        style: SegmentStyle,
        fill: String,
        #[serde(default)]
        unlit: Option<String>,
    },
    /// Cells that roll through a ring of characters, drawn in a font.
    Reel(Reel),
}

/// Segment layout of a segment display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum SegmentStyle {
    /// 14 segments plus dot: letters and digits.
    Alpha14,
    /// 7 segments plus dot: digits and `-`.
    Numeric7,
}

/// Vector shapes, in the layer's local coordinate space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Shape {
    /// `[x, y, width, height]`
    Rect([f64; 4]),
    /// `[cx, cy, radius]`
    Circle([f64; 3]),
    /// SVG path data (`"M 0 0 L 10 0 L 5 8 Z"`): lines, curves and arcs.
    Path(PathData),
}

/// An outline drawn along a shape's edge, centered on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Stroke {
    /// `#RRGGBB` or `#RRGGBBAA`
    pub color: String,
    /// Line width in the layer's units, above 0; 1 by default.
    #[serde(default = "default_scale")]
    pub width: f64,
}

/// An animatable layer property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Property {
    X,
    Y,
    Opacity,
    Scale,
    ScaleX,
    ScaleY,
    Rotation,
    /// The text of a text layer; bindable, not animatable.
    Text,
    /// The font style of a text layer; bindable, not animatable.
    Font,
    /// Sprite sheet cell of an image layer: rounded down and clamped to
    /// the sheet, so a linear key from 0 to n steps through n cells.
    Frame,
    /// Loudness of an audio layer or a group's subtree.
    Gain,
    /// Whether the layer (and its subtree) shows and sounds; bindable,
    /// not animatable. Bound, it is on when the binding's number is not 0.
    Visible,
}

impl Property {
    /// Whether the property holds a number; only those can be keyframed.
    pub fn is_numeric(self) -> bool {
        !matches!(self, Property::Text | Property::Font | Property::Visible)
    }
}

impl Show {
    /// Every trigger name the show listens to: what enters its scenes,
    /// what starts its timelines and what plays or stops its sounds, in
    /// any layer tree. What a host can offer as the show's actions.
    pub fn triggers(&self) -> std::collections::BTreeSet<String> {
        fn timelines(layers: &[Layer], out: &mut std::collections::BTreeSet<String>) {
            for layer in layers {
                for timeline in &layer.timelines {
                    out.extend(timeline.trigger.iter().map(str::to_owned));
                }
                match &layer.kind {
                    LayerKind::Audio { trigger, stop, .. } => {
                        out.extend(trigger.iter().chain(stop.iter()).map(str::to_owned));
                    }
                    LayerKind::Digits {
                        display: DigitDisplay::Reel(reel),
                        ..
                    } => out.extend(reel.spin.iter().map(str::to_owned)),
                    _ => {}
                }
                timelines(layer.children(), out);
            }
        }
        let mut out = std::collections::BTreeSet::new();
        for scene in &self.scenes {
            out.extend(scene.trigger.iter().map(str::to_owned));
        }
        for layers in self.layer_trees() {
            timelines(layers, &mut out);
        }
        out
    }

    /// Every layer tree of the show: its own layers, then each scene's.
    pub fn layer_trees(&self) -> impl Iterator<Item = &[Layer]> {
        std::iter::once(self.layers.as_slice())
            .chain(self.scenes.iter().map(|s| s.layers.as_slice()))
    }
}

impl Layer {
    /// The layers nested in this one: a group's children, else none.
    pub fn children(&self) -> &[Layer] {
        match &self.kind {
            LayerKind::Group { children, .. } => children,
            _ => &[],
        }
    }

    /// The property's value as authored on this layer, or `None` when
    /// this kind of layer does not have the property.
    pub fn base_value(&self, property: Property) -> Option<Value> {
        Some(match (property, &self.kind) {
            (Property::X, _) => Value::Number(self.x),
            (Property::Y, _) => Value::Number(self.y),
            (Property::Opacity, _) => Value::Number(self.opacity),
            (Property::Visible, _) => Value::Bool(self.visible),
            (Property::Scale, _) => Value::Number(self.scale),
            (Property::ScaleX, _) => Value::Number(self.scale_x),
            (Property::ScaleY, _) => Value::Number(self.scale_y),
            (Property::Rotation, _) => Value::Number(self.rotation),
            (Property::Text, LayerKind::Text { text, .. } | LayerKind::Digits { text, .. }) => {
                Value::Text(text.clone())
            }
            (Property::Font, LayerKind::Text { font, .. }) => Value::Text(font.clone()),
            (Property::Frame, LayerKind::Image { frame, .. }) => Value::Number(*frame),
            (Property::Gain, LayerKind::Group { gain, .. } | LayerKind::Audio { gain, .. }) => {
                Value::Number(*gain)
            }
            (Property::Text | Property::Font | Property::Frame | Property::Gain, _) => return None,
        })
    }
}

/// A permanent wiring of a property to a variable, evaluated every frame.
///
/// Numeric properties take `variable * scale + offset`. The `text`
/// property takes the variable as text: numbers get `scale`/`offset`
/// applied, then `format`. The `font` property takes the variable as a
/// font style name.
///
/// With `map`, the variable's value (as text: `1`, `2.5`, `true`, ...)
/// is looked up first and the mapped value, or `default` when it is not
/// listed, takes the variable's place. Without either, the binding does
/// not apply and the property keeps its base value. The order is:
/// `debounce`, `map`, `threshold`, `scale` and `offset`, `transition`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Binding {
    pub property: Property,
    pub variable: String,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub offset: f64,
    /// How a number becomes text (text bindings only).
    #[serde(default)]
    pub format: NumberFormat,
    /// Replace the variable's value by looking it up here.
    #[serde(default)]
    pub map: Option<BTreeMap<String, Value>>,
    /// Value for variable values `map` does not list.
    #[serde(default)]
    pub default: Option<Value>,
    /// A level: the value becomes 1 at or above it and 0 below, before
    /// `scale` and `offset` apply. A lamp that lights when a brightness
    /// passes a half, a `visible` that follows a level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// Seconds a new value has to hold before it reaches the property;
    /// changes shorter than that (a strobing lamp, a bouncing switch)
    /// never show. When a show loads or a scene is entered the value
    /// applies at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce: Option<f64>,
    /// Ease toward a new value instead of jumping to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
}

/// How a bound property moves when its binding's value changes: from the
/// value it has now to the new one over `duration` seconds.
///
/// What is eased is the binding's output (after `map`, `scale` and
/// `offset`), so it has to be a number: on a `text` binding numbers count
/// up or down before they are formatted (whole numbers stay whole on the
/// way), any other text jumps. A change while a transition runs starts a
/// new one from the value reached so far. When a show loads or a scene is
/// entered, properties start at their value; nothing eases in.
///
/// With `wrap` the value lives on a ring of that size (360 for an angle,
/// 10 for a sheet with a frame per digit) and `direction` picks the way
/// round.
///
/// A transition is a small timeline played on every change. `duration`
/// and `ease` say how a move progresses; `offset` adds keyed motion on top,
/// in the value's own units, so it is the same size however far the move
/// goes (a reel settling against its stop); `step` plays a large change as
/// several moves in a row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Transition {
    /// Seconds a change takes; above 0.
    pub duration: f64,
    #[serde(default)]
    pub ease: Easing,
    /// Size of the ring the value lives on; above 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<f64>,
    /// Which way round a wrapped value goes; needs `wrap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Size of one move, above 0: a larger change plays as several moves in
    /// a row, each with its own `duration`, `ease` and `offset`, the last
    /// one shorter when the change is no whole number of steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// Motion added on top of a move, along its direction of travel: keys
    /// like a timeline track's, `t` in seconds from the start of the move,
    /// `v` in the value's units. Has to start and end at 0. A move lasts as
    /// long as the longer of `duration` and these keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offset: Vec<Key>,
}

/// Which way round a wrapped [`Transition`] goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Direction {
    /// Whichever way is shorter; forward on a tie.
    #[default]
    Shortest,
    /// Always toward higher values: 9 to 0 rolls on through the wrap.
    Forward,
    /// Always toward lower values.
    Backward,
}

impl Transition {
    /// Seconds one move takes: `duration`, or the `offset` when it runs
    /// longer.
    pub fn move_time(&self) -> f64 {
        let offset_end = self.offset.last().map_or(0.0, |k| k.t);
        self.duration.max(offset_end)
    }

    /// The value `elapsed` seconds into a change from `start` to `target`.
    pub fn value_at(&self, start: f64, target: f64, elapsed: f64) -> f64 {
        let on_ring = |v: f64| self.wrap.map_or(v, |wrap| v.rem_euclid(wrap));
        let delta = match self.wrap {
            None => target - start,
            Some(wrap) => {
                let forward = (target - start).rem_euclid(wrap);
                match self.direction.unwrap_or_default() {
                    Direction::Shortest if forward > wrap / 2.0 => forward - wrap,
                    Direction::Shortest | Direction::Forward => forward,
                    Direction::Backward if forward == 0.0 => 0.0,
                    Direction::Backward => forward - wrap,
                }
            }
        };
        let distance = delta.abs();
        // One move for the whole change, unless `step` divides it.
        let unit = self
            .step
            .filter(|step| *step < distance)
            .unwrap_or(distance);
        // Tolerant of 3.0000000001 steps being three.
        let moves = if unit > 0.0 {
            (distance / unit - 1e-9).ceil().max(1.0)
        } else {
            1.0
        };
        let period = self.move_time();
        // Decided by time, not by progress: an ease that overshoots passes
        // 1 on the way. Exactly the target, not a rounding step from it.
        if distance == 0.0 || elapsed >= moves * period {
            return on_ring(target);
        }
        let index = (elapsed.max(0.0) / period).floor().min(moves - 1.0);
        let local = elapsed.max(0.0) - index * period;
        let covered = index * unit;
        let length = (distance - covered).min(unit);
        let along = covered + length * self.ease.apply(local / self.duration);
        let offset = sample_keys(&self.offset, local).unwrap_or(0.0);
        on_ring(start + delta.signum() * (along + offset))
    }
}

/// Number to text conversion for text bindings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum NumberFormat {
    /// Shortest form: `1500`, `2.5`.
    #[default]
    Plain,
    /// Rounded to an integer with comma thousands separators: `1,500`.
    Thousands,
}

impl NumberFormat {
    pub fn format(self, n: f64) -> String {
        match self {
            NumberFormat::Plain => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", n as i64)
                } else {
                    format!("{n}")
                }
            }
            NumberFormat::Thousands => {
                let digits = (n.round().abs() as u64).to_string();
                let mut out = String::new();
                for (i, d) in digits.chars().enumerate() {
                    if i > 0 && (digits.len() - i).is_multiple_of(3) {
                        out.push(',');
                    }
                    out.push(d);
                }
                if n.round() < 0.0 {
                    out.insert(0, '-');
                }
                out
            }
        }
    }
}

fn default_scale() -> f64 {
    1.0
}

/// A keyframed animation over one or more properties of its layer.
///
/// A timeline runs when it is `autoplay` and the show loads, or when the
/// host fires its `trigger`. While running it owns the properties it
/// animates: timeline values override bindings, which override base values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Timeline {
    pub name: String,
    /// Trigger name, or list of names, that (re)starts this timeline.
    #[serde(default)]
    pub trigger: Triggers,
    #[serde(default)]
    pub autoplay: bool,
    /// Repeat forever. Cannot be combined with `repeat`.
    #[serde(default, rename = "loop")]
    pub looping: bool,
    /// Seconds to wait after starting before the first key plays; the
    /// timeline does not own its properties meanwhile. Loops and repeats
    /// do not wait again.
    #[serde(default)]
    pub delay: f64,
    /// Number of plays (fractions allowed: 2.5 stops halfway through the
    /// third); once when omitted.
    #[serde(default)]
    pub repeat: Option<f64>,
    /// Trigger fired when the timeline finishes (never for loops).
    #[serde(default)]
    pub on_end: Option<String>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Track {
    pub property: Property,
    pub keys: Vec<Key>,
}

/// A keyframe: at time `t` (seconds) the property reaches value `v`,
/// approached with `ease` from the previous key.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Key {
    pub t: f64,
    pub v: f64,
    #[serde(default)]
    pub ease: Easing,
}

/// Sample `keys` at `time` seconds: the first value before the first key,
/// the last after the last, eased in between. `None` without keys.
pub(crate) fn sample_keys(keys: &[Key], time: f64) -> Option<f64> {
    let first = keys.first()?;
    if time <= first.t {
        return Some(first.v);
    }
    let last = keys.last()?;
    if time >= last.t {
        return Some(last.v);
    }
    let next_idx = keys.iter().position(|k| k.t > time)?;
    let a = &keys[next_idx - 1];
    let b = &keys[next_idx];
    let span = b.t - a.t;
    let t = if span <= 0.0 {
        1.0
    } else {
        (time - a.t) / span
    };
    Some(a.v + (b.v - a.v) * b.ease.apply(t))
}

impl Track {
    /// Sample the track at `time` seconds from timeline start.
    pub fn sample(&self, time: f64) -> Option<f64> {
        sample_keys(&self.keys, time)
    }

    /// End time of the track's last key, 0.0 when empty.
    pub fn duration(&self) -> f64 {
        self.keys.last().map(|k| k.t).unwrap_or(0.0)
    }
}

impl Timeline {
    /// Duration of the longest track: one play.
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .map(Track::duration)
            .fold(0.0_f64, f64::max)
    }

    /// Time after its delay at which a non-looping timeline finishes.
    pub fn play_time(&self) -> f64 {
        self.duration() * self.repeat.unwrap_or(1.0).max(0.0)
    }

    /// Where within one play the tracks are sampled, `elapsed` seconds
    /// after the delay; `None` while still delayed.
    pub fn local_time(&self, elapsed: f64) -> Option<f64> {
        if elapsed < 0.0 {
            return None;
        }
        let duration = self.duration();
        if self.repeat.is_some() && duration > 0.0 && elapsed < self.play_time() {
            return Some(elapsed % duration);
        }
        Some(elapsed)
    }
}

/// Parse a `#RRGGBB` / `#RRGGBBAA` color into RGBA bytes.
pub(crate) fn parse_color(s: &str) -> Option<[u8; 4]> {
    let hex = s.strip_prefix('#')?;
    let parse = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        6 => Some([parse(0)?, parse(2)?, parse(4)?, 0xFF]),
        8 => Some([parse(0)?, parse(2)?, parse(4)?, parse(6)?]),
        _ => None,
    }
}
