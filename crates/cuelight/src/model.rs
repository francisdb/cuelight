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
    /// Values the show animates itself, read by bindings the way a
    /// variable is. For motion that belongs to the content rather than
    /// to whatever is driving it.
    #[serde(default)]
    pub values: BTreeMap<String, ShowValue>,
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

/// One asset name, or several for a layer to pick between: written as a
/// name or a list of names.
///
/// A layer that names several plays one of them per play, chosen by its
/// [`Pick`]. It is the same idea whatever the asset: a sound with three
/// recordings of the same knock, a layer with a folder of clips to show
/// between rounds, an idle animation that should not look like a loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choice(pub Vec<String>);

impl Choice {
    /// A choice of exactly one name.
    pub fn one(name: impl Into<String>) -> Choice {
        Choice(vec![name.into()])
    }

    /// The name at `index`, or nothing when there are none.
    pub fn get(&self, index: usize) -> &str {
        self.0.get(index).map_or("", String::as_str)
    }

    /// The first name, or `""`.
    pub fn first(&self) -> &str {
        self.get(0)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

impl Serialize for Choice {
    /// Written back the way it is usually authored: one name, or a list.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => serializer.serialize_str(one),
            many => many.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Choice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Form {
            One(String),
            Many(Vec<String>),
        }
        Ok(Choice(match Option::<Form>::deserialize(deserializer)? {
            None => Vec::new(),
            Some(Form::One(name)) => vec![name],
            Some(Form::Many(names)) => names,
        }))
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Choice {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Choice".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "An asset name, or a list of names to pick between; see `pick`.",
            "anyOf": [
                { "type": "string" },
                { "type": "array", "items": { "type": "string" } }
            ]
        })
    }
}

/// Which of a layer's several assets a play uses.
///
/// Every one of these is a function of how many times the layer has
/// played, never of a running dice roll, so a show plays the same way
/// twice and a host that seeds the engine
/// ([`Engine::set_seed`](crate::Engine::set_seed)) decides how much it
/// varies between runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Pick {
    /// The next one each play, wrapping round at the end.
    #[default]
    InOrder,
    /// Any of them, which may be the one that just played.
    Random,
    /// All of them in a scrambled order, then scrambled again: varied,
    /// but nothing is skipped and nothing repeats until the rest have had
    /// their turn.
    Shuffle,
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
    /// A copy of the text drawn behind it, offset.
    #[serde(default)]
    pub shadow: Option<Shadow>,
}

/// A copy of the text drawn behind it, offset by a few pixels: the
/// ordinary way to keep text legible over a moving picture.
///
/// What is drawn is the text's whole silhouette, `border` included, in one
/// color. A shadow of a bordered glyph is therefore the same shape as the
/// glyph, which is what makes it read as a shadow rather than a second
/// outline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Shadow {
    /// `#RRGGBB` or `#RRGGBBAA`
    pub color: String,
    /// How far behind the text it sits, `[x, y]` in canvas pixels. Down
    /// and to the right is positive; both may be negative.
    pub offset: [f64; 2],
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
    /// Let this layer (and a group's subtree) draw past the canvas.
    ///
    /// A show is authored against a fixed canvas and a host fits that
    /// canvas into whatever surface it has, painting the area around it
    /// with the show's `background`. That is the right default: it keeps
    /// the canvas meaning exactly what it means for layout. It is the
    /// wrong answer for a backdrop whose job is to reach the edges, which
    /// on a wider display sits in two bars of flat colour with nothing
    /// the show can say about them.
    ///
    /// Marked here, only the clip changes: coordinates are still authored
    /// against the canvas, which stays a safe area for everything placed
    /// exactly. A show cannot know how far it will be asked to stretch,
    /// so anything that bleeds has to be drawn generously.
    ///
    /// It does nothing where the frame *is* the canvas: an offscreen
    /// render, `pixel_perfect` scaling, a `dots` pass, or an output mode
    /// other than `rgb`, all of which draw the canvas at its own size
    /// first. There is no area outside a dot matrix to reach into.
    #[serde(default)]
    pub overflow: bool,
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
    /// given. The fill is a color, or a gradient that stays smooth at any
    /// size where a show would otherwise carry a small image of one.
    Shape {
        shape: Shape,
        fill: Fill,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Stroke>,
    },
    /// Artwork the host registered under `image`: pixels through
    /// [`Engine::set_image`](crate::Engine::set_image), or vector artwork
    /// through [`Engine::set_vector`](crate::Engine::set_vector), which
    /// the loader does for every `assets/*.svg`. Drawn with its top-left
    /// corner at the layer's x/y unless the layer has an `anchor`; what
    /// is not registered yet is skipped.
    ///
    /// One layer kind for both, since the asset says how to draw itself
    /// and everything else about a layer of artwork is the same. `type`
    /// and the name may still be written as `vector`, which older shows
    /// do; `sheet` and `frame` are for pixels, and say so at load on
    /// vector artwork.
    #[serde(alias = "vector")]
    Image {
        #[serde(alias = "vector")]
        image: String,
        /// Destination size `[width, height]`; the image's natural size
        /// (one cell's size with a `sheet`) when omitted.
        #[serde(default)]
        size: Option<[f64; 2]>,
        /// Treat the image as a grid of equally sized cells and draw one:
        /// the one the `frame` property selects. Pixels only.
        #[serde(default)]
        sheet: Option<Sheet>,
        /// Base cell index for sheets (row-major, 0 is the top-left cell).
        #[serde(default)]
        frame: f64,
        /// Color the artwork is multiplied by, `#RRGGBB` or `#RRGGBBAA`:
        /// white leaves it alone, a color stains it (a lamp behind white
        /// art, a worn look, one sprite or icon in several colors).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tint: Option<String>,
        /// Tile the artwork across `size` instead of stretching to it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repeat: Option<Tile>,
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
    /// A video the host registered under `video` with its duration and
    /// size ([`Engine::set_video`](crate::Engine::set_video)), played the
    /// way a sound is: on `trigger` or at load with `autoplay`, after
    /// `delay`, looping or repeating, firing `on_end`. The engine decodes
    /// nothing: it reports what should be showing and at which position
    /// ([`Engine::videos`](crate::Engine::videos)), and draws whatever
    /// frame the host last registered as an image under the video's name.
    Video {
        video: Choice,
        /// Which of several videos a play shows; ignored for one.
        #[serde(default)]
        pick: Pick,
        /// Drawn size `[width, height]`; the video's own when omitted.
        #[serde(default)]
        size: Option<[f64; 2]>,
        /// Trigger name, or list of names, that plays it.
        #[serde(default)]
        trigger: Triggers,
        /// Play when the show loads or the scene is entered.
        #[serde(default)]
        autoplay: bool,
        /// Repeat forever. Cannot be combined with `repeat`.
        #[serde(default, rename = "loop")]
        looping: bool,
        /// Seconds between the trigger and the first frame.
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
        /// Least seconds between one play starting and the next; a
        /// trigger that comes sooner is dropped. 0 (default) never drops.
        #[serde(default)]
        rest: f64,
        /// What the trigger does while a clip is already showing:
        /// `restart` (default), `ignore` or `queue`. Not `overlap`: a
        /// layer shows one picture at a time.
        #[serde(default)]
        retrigger: Retrigger,
        /// How many plays may wait their turn with `queue`. Default 4.
        #[serde(default = "default_voices")]
        voices: u32,
        /// Loudness of the clip's own soundtrack, if the host registered
        /// one: 0 to 1 and above, times the gains of the groups above it.
        /// 0 plays the picture silently. A normal numeric property:
        /// bindable and animatable.
        #[serde(default = "default_scale")]
        gain: f64,
        /// Name of the bus the clip's sound plays through; hosts route
        /// buses to outputs. [`MAIN_BUS`] when omitted, so every sound is
        /// on one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bus: Option<String>,
        /// Step back while something else is sounding; see [`Duck`]. A
        /// clip with a soundtrack wants this as much as a sound does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duck: Option<Duck>,
    },
    /// A sound the host registered under `sound` with its duration
    /// ([`Engine::set_sound`](crate::Engine::set_sound)), played the way a
    /// timeline is: on `trigger` or at load with `autoplay`, after `delay`,
    /// looping or repeating, firing `on_end`. Draws nothing. What plays is
    /// reported by [`Engine::voices`](crate::Engine::voices); the engine
    /// never touches samples.
    Audio {
        sound: Choice,
        /// Which of several sounds a play uses; ignored for one.
        #[serde(default)]
        pick: Pick,
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
        /// Least seconds between one play starting and the next; a
        /// trigger that comes sooner is dropped. 0 (default) never drops.
        #[serde(default)]
        rest: f64,
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
        /// outputs. [`MAIN_BUS`] when omitted, so every sound is on one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bus: Option<String>,
        /// Step back while something else is sounding; see [`Duck`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duck: Option<Duck>,
    },
}

fn default_voices() -> u32 {
    4
}

/// Which registry the content of a playhead comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MediaKind {
    Sound,
    Video,
}

/// The playhead of a layer whose content has a length: what starts it,
/// how long it goes on and what it says when it ends. A sound and a video
/// carry the same one, so the engine runs both through the same code.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Media<'a> {
    pub kind: MediaKind,
    /// The assets the host registered, by name: one, or several to pick
    /// between.
    pub names: &'a Choice,
    /// Which of `names` a play uses.
    pub pick: Pick,
    pub trigger: &'a Triggers,
    pub stop: &'a Triggers,
    pub autoplay: bool,
    pub looping: bool,
    pub delay: f64,
    pub repeat: Option<f64>,
    pub on_end: Option<&'a str>,
    /// What a trigger does while it already plays.
    pub retrigger: Retrigger,
    /// Least seconds between plays; 0 never drops a trigger.
    pub rest: f64,
    /// How many plays may run at once; one for a video.
    pub voices: u32,
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
    /// Let the play finish, then play: the trigger waits its turn, up to
    /// `voices` of them waiting at once.
    Queue,
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
            model: None,
            kelvin: None,
            heating: None,
            cooling: None,
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
        /// Degrees the cells lean, as a shear rather than a rotation, so
        /// the baseline stays level. Most real displays lean about ten,
        /// and 45 is as far as it goes. Positive leans the tops to the
        /// right.
        #[serde(default)]
        slant: f64,
        /// Segment width as a share of the cell's shorter side, 0.1 by
        /// default and 0.2 at most. The gaps between segments follow it,
        /// so a fat display stays legible instead of running together,
        /// and past that cap the bars would meet.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thickness: Option<f64>,
        /// A halo around lit segments, in their own colour. Unlit ones
        /// never glow.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        glow: Option<Glow>,
    },
    /// Cells that roll through a ring of characters, drawn in a font.
    Reel(Reel),
}

/// A halo around the lit segments of a display.
///
/// Gas-discharge and fluorescent displays spill light around every lit
/// segment, and a panel recreated without it looks wrong however right
/// the digits are.
///
/// Drawn as the segment again, a few times, each grown and fainter than
/// the last: a halo rather than a true blur, which nothing on the GPU can
/// give us yet (see the note on text shadows). At the sizes a display is
/// drawn it reads the same.
///
/// Drawn as one picture, so that where two segments' halos meet the
/// brighter of them shows: a halo is never brighter than the segment
/// casting it, however many of them overlap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Glow {
    /// How far it reaches beyond the segment, as a share of the cell's
    /// width. About 0.05 is a rim and about 0.25 a lit panel; past a
    /// third of a cell it is a wash rather than a display.
    pub size: f64,
    /// How bright it is where it leaves the segment, 0 to 1, where 1 is
    /// as bright as the segment itself.
    #[serde(default = "default_glow_strength")]
    pub strength: f64,
}

fn default_glow_strength() -> f64 {
    0.5
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
///
/// Untagged, so a rect can carry a corner radius beside it
/// (`{ "rect": [0, 0, 8, 4], "radius": 2 }`) rather than nesting it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Shape {
    Rect {
        /// `[x, y, width, height]`
        rect: [f64; 4],
        /// Corner radius, clamped to half the shorter side, so a large
        /// one gives a pill. All four corners; absent is square.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<f64>,
    },
    Circle {
        /// `[cx, cy, radius]`
        circle: [f64; 3],
    },
    Path {
        /// SVG path data (`"M 0 0 L 10 0 L 5 8 Z"`): lines, curves and arcs.
        path: PathData,
    },
}

// Described by hand for the same reason it is read by hand: an untagged
// enum generates `anyOf` with no `additionalProperties`, so an editor
// would not flag a misspelt `raduis`, a `radius` on a circle, or a rect
// and a circle in one object. `oneOf` with each form closed says what
// the loader accepts.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for Shape {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Shape".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        concat!(module_path!(), "::Shape").into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let numbers = |n: usize, description: &str| {
            serde_json::json!({
                "description": description,
                "type": "array",
                "items": { "type": "number", "format": "double" },
                "minItems": n,
                "maxItems": n
            })
        };
        let closed = |name: &str, shape: serde_json::Value, extra: serde_json::Value| {
            let mut properties = serde_json::Map::new();
            properties.insert(name.to_owned(), shape);
            if let serde_json::Value::Object(more) = extra {
                properties.extend(more);
            }
            serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": [name],
                "additionalProperties": false
            })
        };
        let radius = serde_json::json!({
            "radius": {
                "description": "Corner radius, clamped to half the shorter side, so a \
                                large one gives a pill. All four corners; absent is square.",
                "type": ["number", "null"],
                "format": "double"
            }
        });
        let none = serde_json::Value::Null;
        schemars::Schema::try_from(serde_json::json!({
            "description": "Vector shapes, in the layer's local coordinate space.\n\n\
                            One of rect, circle or path; a rect may carry a corner \
                            radius beside it.",
            "oneOf": [
                closed("rect", numbers(4, "`[x, y, width, height]`"), radius),
                closed("circle", numbers(3, "`[cx, cy, radius]`"), none.clone()),
                closed(
                    "path",
                    serde_json::to_value(generator.subschema_for::<PathData>())
                        .unwrap_or(serde_json::Value::Bool(true)),
                    none,
                ),
            ]
        }))
        .expect("a schema built from an object literal")
    }
}

// Read by hand rather than as an untagged enum: serde answers a bad
// shape with "data did not match any variant", which says nothing about
// what is wrong with it. Naming the key first means a malformed path
// still reports what the path parser found.
impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            rect: Option<[f64; 4]>,
            #[serde(default)]
            radius: Option<f64>,
            #[serde(default)]
            circle: Option<[f64; 3]>,
            #[serde(default)]
            path: Option<PathData>,
        }
        let raw = Raw::deserialize(deserializer)?;
        match (raw.rect, raw.circle, raw.path) {
            (Some(rect), None, None) => Ok(Shape::Rect {
                rect,
                radius: raw.radius,
            }),
            (None, Some(circle), None) => Ok(Shape::Circle { circle }),
            (None, None, Some(path)) => Ok(Shape::Path { path }),
            (None, None, None) => Err(serde::de::Error::custom(
                "a shape needs one of rect, circle or path",
            )),
            _ => Err(serde::de::Error::custom(
                "a shape takes one of rect, circle or path, not several",
            )),
        }
    }
}

impl Shape {
    /// A rect's corner radius, clamped to what its size allows.
    pub fn corner_radius(rect: [f64; 4], radius: Option<f64>) -> f64 {
        let [_, _, w, h] = rect;
        radius
            .unwrap_or(0.0)
            .max(0.0)
            .min(w.abs() / 2.0)
            .min(h.abs() / 2.0)
    }
}

/// What a shape is filled with: one color, or a gradient.
///
/// Untagged, so `"fill": "#FF0000"` keeps working and
/// `"fill": { "radial": { ... } }` is the other one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum Fill {
    /// `#RRGGBB` or `#RRGGBBAA`.
    Color(String),
    Gradient(Gradient),
}

impl Default for Fill {
    fn default() -> Self {
        Fill::Color("#FFFFFF".to_owned())
    }
}

/// A smooth run of colors across a shape, in the shape's own space, so it
/// travels with whatever moves the shape.
///
/// Glows, vignettes, the shading that makes a drum look round and the
/// sheen on glass are all gradients, and carrying them as small raster
/// images means the same workaround in every show and blurring whenever
/// one is scaled up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Gradient {
    /// Runs along the line from `from` to `to`, and holds its end colors
    /// beyond either end.
    Linear {
        from: [f64; 2],
        to: [f64; 2],
        stops: Vec<Stop>,
    },
    /// Runs out from `center` to `radius`, and holds its last color
    /// beyond it.
    Radial {
        center: [f64; 2],
        radius: f64,
        stops: Vec<Stop>,
    },
}

impl Gradient {
    pub fn stops(&self) -> &[Stop] {
        match self {
            Gradient::Linear { stops, .. } | Gradient::Radial { stops, .. } => stops,
        }
    }
}

/// One color of a gradient, `at` a fraction of the way along it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Stop {
    /// 0 at the start of the gradient, 1 at its end.
    pub at: f64,
    /// `#RRGGBB` or `#RRGGBBAA`.
    pub color: String,
}

/// The bus a sound is on when it names none.
///
/// Every sound is on a bus, so a show that never mentions one can still
/// be ducked under: give the bed a bus of its own and point its `duck` at
/// this.
pub const MAIN_BUS: &str = "main";

/// Step this layer back while something on another bus is sounding.
///
/// A bed under clips that speak over it has to get out of the way and
/// come back, which is the ordinary arrangement whenever there is music
/// under anything that talks. `gain` is bindable and a transition can
/// ease it, but nothing in a show can see that something else is
/// sounding, so without this a host has to watch the engine's voices and
/// feed a variable back, putting show structure outside the show.
///
/// It multiplies like every other gain, so it composes with bindings and
/// with the gain of the groups above rather than fighting them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Duck {
    /// The bus to listen to. Anything sounding on it ducks this layer;
    /// the layer's own plays never do.
    pub under: String,
    /// Gain multiplier while that bus sounds. 0.1 is a tenth.
    pub to: f64,
    /// Seconds to go down. 0 (the default) drops at once, which is what
    /// makes room in time for the first word.
    #[serde(default)]
    pub attack: f64,
    /// Seconds to come back up once the bus falls silent. The part that
    /// has to be smooth.
    #[serde(default)]
    pub release: f64,
}

/// Repeat a layer's content across its `size` instead of stretching it
/// to fit.
///
/// A pattern otherwise has to be written out: a checkerboard is a hundred
/// and twenty eight rectangles, and a reader cannot tell that from a
/// hundred and twenty eight unrelated ones. The same want turns up for a
/// grid, a scanline overlay, a floor, a wall of dots, any texture meant to
/// cover whatever it is put behind.
///
/// Tiling happens in the layer's own space, so a rotating or scaled group
/// carries the pattern with it rather than sliding underneath it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Tile {
    /// Size of one tile in the layer's units. The content's own size when
    /// omitted, which is what "repeat this at its natural size" means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    /// Where the pattern starts, in the layer's units. Animate it and the
    /// texture scrolls under a fixed window.
    #[serde(default)]
    pub offset: [f64; 2],
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
    /// The video a video layer plays; bindable, not animatable. Binding
    /// it lets one layer show whatever it is pointed at, rather than
    /// needing a layer per clip.
    Video,
    /// The sound an audio layer plays; bindable, not animatable, and the
    /// mirror of `video`. Binding it lets one layer sound whatever it is
    /// pointed at: a bed that follows the state a show is in, rather than
    /// a layer per track each having to stop the others.
    Sound,
    /// Sprite sheet cell of an image layer: rounded down and clamped to
    /// the sheet, so a linear key from 0 to n steps through n cells.
    Frame,
    /// Where a tiled image's pattern starts, along x and y, in the
    /// layer's units. Animating one scrolls the texture under the layer.
    TileX,
    TileY,
    /// Loudness of an audio layer or a group's subtree.
    Gain,
    /// Whether the layer (and its subtree) shows and sounds; bindable,
    /// not animatable. Bound, it is on when the binding's number is not 0.
    Visible,
    /// The color an image layer is multiplied by, as `#RRGGBB` or
    /// `#RRGGBBAA`; bindable, not animatable. An empty value leaves the
    /// image alone.
    Tint,
}

impl Property {
    /// Whether the property holds a number; only those can be keyframed.
    pub fn is_numeric(self) -> bool {
        !matches!(
            self,
            Property::Text
                | Property::Font
                | Property::Visible
                | Property::Tint
                | Property::Video
                | Property::Sound
        )
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
                if let Some(media) = layer.kind.media() {
                    out.extend(
                        media
                            .trigger
                            .iter()
                            .chain(media.stop.iter())
                            .map(str::to_owned),
                    );
                }
                if let LayerKind::Digits {
                    display: DigitDisplay::Reel(reel),
                    ..
                } = &layer.kind
                {
                    out.extend(reel.spin.iter().map(str::to_owned));
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

    /// Whether anything in the show can make a sound: an audio layer, in
    /// its own layers or any scene's. A host that opens a sound device
    /// can skip doing so entirely for a show that is silent by
    /// construction.
    ///
    /// A video layer is not counted, even though a clip can be heard: it
    /// is heard only if the host registers a sound under the video's name,
    /// and the show's own document cannot say whether it will. A host that
    /// registers a clip's soundtrack knows it did, and should open a sound
    /// device on that ground rather than on this answer.
    pub fn has_sound(&self) -> bool {
        fn any(layers: &[Layer]) -> bool {
            layers
                .iter()
                .any(|layer| matches!(layer.kind, LayerKind::Audio { .. }) || any(layer.children()))
        }
        self.layer_trees().any(any)
    }

    /// Every layer tree of the show: its own layers, then each scene's.
    pub fn layer_trees(&self) -> impl Iterator<Item = &[Layer]> {
        std::iter::once(self.layers.as_slice())
            .chain(self.scenes.iter().map(|s| s.layers.as_slice()))
    }
}

impl LayerKind {
    /// The playhead of this layer, when its content has a length: a sound
    /// or a video.
    pub fn media(&self) -> Option<Media<'_>> {
        match self {
            LayerKind::Audio {
                sound,
                pick,
                trigger,
                autoplay,
                looping,
                delay,
                repeat,
                on_end,
                stop,
                retrigger,
                voices,
                rest,
                ..
            } => Some(Media {
                kind: MediaKind::Sound,
                names: sound,
                pick: *pick,
                trigger,
                stop,
                autoplay: *autoplay,
                looping: *looping,
                delay: *delay,
                repeat: *repeat,
                on_end: on_end.as_deref(),
                retrigger: *retrigger,
                voices: *voices,
                rest: *rest,
            }),
            LayerKind::Video {
                video,
                pick,
                trigger,
                autoplay,
                looping,
                delay,
                repeat,
                on_end,
                stop,
                retrigger,
                voices,
                rest,
                ..
            } => Some(Media {
                kind: MediaKind::Video,
                names: video,
                pick: *pick,
                trigger,
                stop,
                autoplay: *autoplay,
                looping: *looping,
                delay: *delay,
                repeat: *repeat,
                on_end: on_end.as_deref(),
                retrigger: *retrigger,
                voices: *voices,
                rest: *rest,
            }),
            _ => None,
        }
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
            (Property::Video, LayerKind::Video { video, .. }) => {
                Value::Text(video.first().to_owned())
            }
            (Property::Sound, LayerKind::Audio { sound, .. }) => {
                Value::Text(sound.first().to_owned())
            }
            (Property::Frame, LayerKind::Image { frame, .. }) => Value::Number(*frame),
            (
                Property::TileX,
                LayerKind::Image {
                    repeat: Some(tile), ..
                },
            ) => Value::Number(tile.offset[0]),
            (
                Property::TileY,
                LayerKind::Image {
                    repeat: Some(tile), ..
                },
            ) => Value::Number(tile.offset[1]),
            (Property::Tint, LayerKind::Image { tint, .. }) => {
                Value::Text(tint.clone().unwrap_or_default())
            }
            (
                Property::Gain,
                LayerKind::Group { gain, .. }
                | LayerKind::Audio { gain, .. }
                | LayerKind::Video { gain, .. },
            ) => Value::Number(*gain),
            (
                Property::Text
                | Property::Font
                | Property::Frame
                | Property::Gain
                | Property::Tint
                | Property::Video
                | Property::Sound
                | Property::TileX
                | Property::TileY,
                _,
            ) => return None,
        })
    }
}

/// A condition on a variable that starts something when it becomes true.
///
/// A host that only sends states - lamps going on and off, a score
/// crossing a mark, a mode taking a value - has no trigger to fire, and
/// without this every such host has to watch its own variables and invent
/// trigger names for them, which is show logic living outside the show.
///
/// The value is read the way a binding reads one: `map` replaces it when
/// it lists it, then `threshold` turns a number into 0 or 1. True is any
/// value that is not 0, and what starts the timeline is *becoming* true,
/// so a lamp that stays on plays it once rather than every frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct When {
    pub variable: String,
    /// Replace the variable's value by looking it up here, as on a
    /// binding: `{ "multiball": 1 }` is true exactly in that mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<BTreeMap<String, Value>>,
    /// Value for variable values `map` does not list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// A level: the value counts as true at or above it. Without one, any
    /// value that is not 0 is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
}

/// A condition a timeline runs under, rather than starts on.
///
/// Read exactly as a [`When`] is; the difference is what it does with the
/// answer. See [`Timeline::whilst`].
pub type While = When;

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
    /// How many decimal places a number shows, when it becomes text.
    ///
    /// Without it a number prints as short as it can, so a value a
    /// timeline is moving reads `1.4833333333333334` between its keys.
    /// With it the number is rounded to that many places and always
    /// shows them: `1.5` with two is `1.50`.
    ///
    /// Applies after `scale` and `offset`, as the rest of formatting
    /// does, and alongside `format`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u32>,
    /// How a number becomes text (text bindings only).
    #[serde(default)]
    pub format: NumberFormat,
    /// Words in front of the value, and behind it (text bindings only).
    ///
    /// A readout is rarely a bare number: `40%`, `BALL 2`, `2.5 X`,
    /// `LEVEL 12`. Putting them in a layer of their own beside the number
    /// only holds while nothing is centred or right-aligned, since the
    /// number's width changes and the words do not move with it.
    ///
    /// They apply last, to whatever text the binding produces, so a
    /// counting `transition` counts the number and leaves the words
    /// still, and a `map`'s text gets them as much as a number does. A
    /// binding that does not apply (an unset variable, a `map` with
    /// nothing to say and no `default`) leaves the property as it was,
    /// words included.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    /// Words behind the value; see `prefix`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suffix: String,
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
    /// Bend the value against the input instead of scaling it straight.
    ///
    /// Keys are a track's, with the input value where a track has time,
    /// and the same easings between them: below the first key it holds
    /// the first value, above the last it holds the last. A lamp whose
    /// glow wants a gamma curve, a tachometer compressed at the low end,
    /// a loudness in decibels rather than a linear gain.
    ///
    /// This shapes value against input; a [`Transition`]'s `ease` shapes
    /// a change over time. A binding can have both, and they do different
    /// things.
    ///
    /// It applies after `map` and before `scale` and `offset`, so the
    /// curve is written in the variable's own units and `scale` stays the
    /// last change of unit. It cannot be combined with `threshold`, which
    /// is the same job done crudely: a `step` ease says it as a curve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curve: Vec<Key>,
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
    /// Seconds a change takes; above 0. Not used, and not needed, with a
    /// `model`, which decides its own timing.
    #[serde(default)]
    pub duration: f64,
    /// Follow a physical model instead of easing over a duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    /// Temperature of the filament at full power, in kelvin, which is
    /// also the colour it glows there. 2700 by default, a warm white; a
    /// bigger lamp runs hotter and whiter, a small one cooler and redder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kelvin: Option<f64>,
    /// How quickly the filament heats, in seconds: the time it takes to
    /// close about two thirds of the gap to where it is heading. Full
    /// brightness takes roughly five times this. 0.007 by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heating: Option<f64>,
    /// The same going the other way, and always the slower of the two: a
    /// filament loses heat more slowly than the power puts it in. 0.06 by
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooling: Option<f64>,
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
    /// `n` as text, to `decimals` places when asked for.
    pub fn format(self, n: f64, decimals: Option<u32>) -> String {
        let Some(places) = decimals else {
            return self.format_short(n);
        };
        let places = places as usize;
        // Rounded to the last shown place first, so the grouping below
        // sees the number that will be printed, and a value that rounds
        // to zero does not keep a minus sign from the way it came.
        let scale = 10f64.powi(places as i32);
        let n = (n * scale).round() / scale;
        let n = if n == 0.0 { 0.0 } else { n };
        match self {
            NumberFormat::Plain => format!("{n:.places$}"),
            NumberFormat::Thousands => {
                let whole = n.abs().trunc();
                let grouped = Self::group(whole as u64);
                let fraction = format!("{:.places$}", n.abs().fract());
                let sign = if n < 0.0 { "-" } else { "" };
                match places {
                    0 => format!("{sign}{grouped}"),
                    // `fraction` is "0.25": everything after its point.
                    _ => format!("{sign}{grouped}{}", &fraction[1..]),
                }
            }
        }
    }

    /// The comma-grouped digits of a whole number.
    fn group(whole: u64) -> String {
        let digits = whole.to_string();
        let mut out = String::new();
        for (i, d) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(d);
        }
        out
    }

    fn format_short(self, n: f64) -> String {
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
    /// A variable condition that (re)starts it on the rising edge, for
    /// hosts that send states rather than events. The edge belongs to the
    /// variable, not to the scene: leaving a scene and coming back does
    /// not replay it unless the condition turned true while away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<When>,
    /// A variable condition it runs under: it plays while the condition
    /// holds and stops when it stops holding.
    ///
    /// What `when` cannot say. A blink that means "this is lit" should
    /// run for as long as it is lit, and a looping timeline started on an
    /// edge would never stop. Unlike `when`, entering a scene starts it
    /// again, since it describes a state the scene is in rather than
    /// something that happened.
    ///
    /// Stopping is not finishing: it fires no `on_end`.
    #[serde(default, rename = "while", skip_serializing_if = "Option::is_none")]
    pub whilst: Option<While>,
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
    /// Keep the last value instead of giving the properties back.
    ///
    /// A timeline that ends hands its properties back to their binding or
    /// base value, which is what a pulse or a flash wants. A fade *into* a
    /// state wants to stay there, and writing the end value into the base
    /// as well only works while nothing else ever animates that property.
    ///
    /// Held, a finished timeline keeps its properties at their last values
    /// until it is started again or its scene is left, and it ranks below
    /// any timeline still running, so a later flash on the same property
    /// wins while it plays and hands back to the held value afterwards. It
    /// means nothing on a loop, which never finishes.
    #[serde(default)]
    pub hold: bool,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Track {
    pub property: Property,
    pub keys: Vec<Key>,
}

/// A physical model a transition follows instead of an ease.
///
/// A lamp is not a fade. Much of how a panel of lamps looks is how they
/// switch, and a lamp switching is its filament's temperature chasing the
/// power put into it: full brightness in a few tens of milliseconds, most
/// of the light gone as fast when the power goes, then a dim red glow for
/// much longer. A filament re-lit while still warm comes up quicker than
/// a cold one, which no duration and ease can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Model {
    /// A glowing filament. The binding's value is the power put into it,
    /// 0 to 1; a numeric property gets the light that comes out and a
    /// `tint` gets the filament's colour, which reddens as it cools.
    ///
    /// Shaped by `kelvin`, `heating` and `cooling`, so it is any lamp
    /// with a filament rather than one make of bulb.
    Incandescent,
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

/// A value the show owns and animates, named and read like a variable.
///
/// A timeline animates a property of its own layer, so two layers that
/// must move together have to duplicate its keys, and nothing then says
/// they are meant to agree: a scene restarting one, or an edit to one set
/// of keys, parts them silently. A group shares a transform instead, but
/// it scales positions along with everything else, so it only works when
/// every reader sits at the group's origin.
///
/// A show value is the source both of them read. A host that sets a
/// variable of the same name takes it over, so a show can ship with its
/// own motion that a host is free to seize.
///
/// Nothing in a show writes one. Variables are the host's inputs, and
/// content writing them would make ownership ambiguous and allow a
/// variable that drives a timeline that writes that variable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ShowValue {
    /// What moves it, played exactly as a layer's timelines are. Several
    /// of them cover one value in stretches, each started by its own
    /// trigger, the way a layer holds several timelines for one property.
    #[serde(default)]
    pub timelines: Vec<ValueTimeline>,
}

/// One stretch of a show value's motion: a timeline whose keys are the
/// value itself, so it has no tracks and names no property.
///
/// Everything above `keys` means what it means on a
/// [`Timeline`](crate::Timeline), and the two are kept in step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ValueTimeline {
    pub name: String,
    /// What starts it; firing any of several has the same effect.
    #[serde(default, skip_serializing_if = "Triggers::is_empty")]
    pub trigger: Triggers,
    /// Start it when the show loads.
    #[serde(default)]
    pub autoplay: bool,
    /// Repeat for ever; cannot be combined with `repeat`.
    #[serde(default, rename = "loop")]
    pub looping: bool,
    /// Seconds to wait after starting before the first key.
    #[serde(default)]
    pub delay: f64,
    /// Play it this many times; fractions stop part way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<f64>,
    /// Trigger fired when it finishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_end: Option<String>,
    /// Keep the last value instead of handing the value back.
    #[serde(default)]
    pub hold: bool,
    /// Keyframes of the value itself.
    pub keys: Vec<Key>,
}

impl ValueTimeline {
    /// One play, in seconds.
    pub fn duration(&self) -> f64 {
        self.keys.last().map(|k| k.t).unwrap_or(0.0)
    }

    /// Time after its delay at which a non-looping one finishes.
    pub fn play_time(&self) -> f64 {
        self.duration() * self.repeat.unwrap_or(1.0).max(0.0)
    }

    /// Where within one play the keys are sampled, `elapsed` seconds
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

    /// The value `elapsed` seconds after its delay.
    pub fn at(&self, elapsed: f64) -> Option<f64> {
        sample_keys(&self.keys, self.local_time(elapsed)?)
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
