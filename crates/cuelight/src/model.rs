use crate::easing::Easing;
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
}

impl Output {
    /// This output with its unset fields taken from `base`.
    pub fn over(&self, base: &Output) -> Output {
        Output {
            mode: self.mode.or(base.mode),
            tint: self.tint.clone().or_else(|| base.tint.clone()),
            scaling: self.scaling.or(base.scaling),
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
    /// Name of the bitmap font as registered by the host with
    /// [`Engine::set_font`](crate::Engine::set_font) (by convention the
    /// `.fnt` file stem).
    pub file: String,
    /// Glyph color multiplier, `#RRGGBB`; white keeps the font's colors.
    #[serde(default = "default_font_color")]
    pub color: String,
    #[serde(default)]
    pub border: Option<Border>,
}

/// An outline drawn around every glyph.
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
    /// Uniform scale of this layer's own geometry around its x/y origin
    /// (shape coordinates and sizes, image destination size). With an
    /// `anchor` the anchored point stays at x/y while scaling. Not
    /// inherited by group children yet.
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// Which point of the layer's content box sits at its x/y. Without an
    /// anchor, images put their top-left corner there and shapes their
    /// local origin. Not supported on groups.
    #[serde(default)]
    pub anchor: Option<Align>,
    #[serde(default = "default_visible")]
    pub visible: bool,
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
    },
    Shape {
        shape: Shape,
        fill: String,
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Shape {
    /// `[x, y, width, height]`
    Rect([f64; 4]),
    /// `[cx, cy, radius]`
    Circle([f64; 3]),
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
    /// The text of a text layer; bindable, not animatable.
    Text,
    /// The font style of a text layer; bindable, not animatable.
    Font,
    /// Sprite sheet cell of an image layer: rounded down and clamped to
    /// the sheet, so a linear key from 0 to n steps through n cells.
    Frame,
}

impl Property {
    /// Whether the property holds a number; only those can be keyframed.
    pub fn is_numeric(self) -> bool {
        !matches!(self, Property::Text | Property::Font)
    }
}

impl Show {
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
            (Property::Scale, _) => Value::Number(self.scale),
            (Property::Text, LayerKind::Text { text, .. } | LayerKind::Digits { text, .. }) => {
                Value::Text(text.clone())
            }
            (Property::Font, LayerKind::Text { font, .. }) => Value::Text(font.clone()),
            (Property::Frame, LayerKind::Image { frame, .. }) => Value::Number(*frame),
            (Property::Text | Property::Font | Property::Frame, _) => return None,
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
/// not apply and the property keeps its base value.
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Key {
    pub t: f64,
    pub v: f64,
    #[serde(default)]
    pub ease: Easing,
}

impl Track {
    /// Sample the track at `time` seconds from timeline start.
    pub fn sample(&self, time: f64) -> Option<f64> {
        let first = self.keys.first()?;
        if time <= first.t {
            return Some(first.v);
        }
        let last = self.keys.last()?;
        if time >= last.t {
            return Some(last.v);
        }
        let next_idx = self.keys.iter().position(|k| k.t > time)?;
        let a = &self.keys[next_idx - 1];
        let b = &self.keys[next_idx];
        let span = b.t - a.t;
        let t = if span <= 0.0 {
            1.0
        } else {
            (time - a.t) / span
        };
        Some(a.v + (b.v - a.v) * b.ease.apply(t))
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
