use crate::easing::Easing;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A declarative show description: what a `.json` show file deserializes into.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Show {
    pub name: String,
    /// Logical canvas size in pixels `[width, height]`. Hosts scale the
    /// rendered texture; content is authored against this space.
    /// Coordinates have their origin at the top-left corner: x grows
    /// right, y grows down.
    pub size: [u32; 2],
    /// Background color, `#RRGGBB` or `#RRGGBBAA`.
    #[serde(default = "default_background")]
    pub background: String,
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

/// A switchable view of the show: its layers render only while it is the
/// active scene. Entering a scene (again) restarts it: timelines of the
/// previous scene stop, the entered scene's autoplay timelines start at 0.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Scene {
    pub name: String,
    /// Trigger name that enters this scene.
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub layers: Vec<Layer>,
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
pub enum LayerKind {
    Group {
        children: Vec<Layer>,
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
        /// when omitted.
        #[serde(default)]
        size: Option<[f64; 2]>,
    },
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
}

/// Vector shapes, in the layer's local coordinate space.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
pub enum Property {
    X,
    Y,
    Opacity,
    Scale,
}

/// A permanent wiring of a property to a variable, evaluated every frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Binding {
    pub property: Property,
    pub variable: String,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub offset: f64,
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
    /// Trigger name that (re)starts this timeline.
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub autoplay: bool,
    #[serde(default, rename = "loop")]
    pub looping: bool,
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
    /// Duration of the longest track.
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .map(Track::duration)
            .fold(0.0_f64, f64::max)
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
