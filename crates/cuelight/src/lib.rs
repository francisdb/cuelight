//! cuelight: an embeddable multimedia engine.
//!
//! Shows are trees of typed layers whose properties are driven by three
//! kinds of input: **variables** (named values pushed by the host),
//! **triggers** (named events fired by the host) and **timelines**
//! (keyframed property animation described as data).
//!
//! The core contract is four calls:
//!
//! - [`Engine::load_show`]: load a declarative show description (JSON)
//! - [`Engine::set_variable`]: push a named value from the host
//! - [`Engine::trigger`]: fire a named event
//! - [`Engine::advance_frame`]: advance time by `dt` seconds
//!
//! plus obtaining the output. [`Engine::resolved_layers`] is a flat draw
//! list for hosts that draw themselves: the engine only describes what to
//! draw and never touches the GPU. With the `render`
//! feature, [`render::Renderer`] renders offscreen to RGBA pixels and
//! [`render::Presenter`] puts the show on a host surface, fitted and with
//! its output mode applied. [`Engine::voices`] is the same for sound: what
//! should be heard, for an audio backend to play (the engine never touches
//! samples). [`Engine::drain_events`] hands back what the show itself
//! fired.
//!
//! Everything else belongs to hosts and adapters, not this crate.

mod easing;
mod engine;
mod font;
mod lru;
mod model;
#[cfg(feature = "outline-fonts")]
mod outline;
mod output;
mod path;
mod segments;
mod value;

pub use easing::Easing;
pub use engine::{
    Engine, Error, Event, FontData, ImageData, PlacedGlyph, ResolvedLayer, ResolvedShape,
    Transform, Vector, VectorPath, Voice,
};
pub use font::BitmapFont;
pub use model::{
    Align, Binding, Blend, Border, DigitDisplay, Direction, DotShape, Dots, FontStyle, Justify,
    Key, Layer, LayerKind, NumberFormat, Output, OutputMode, Pass, Property, Reel, ReelCells,
    Retrigger, Scaling, Scene, SegmentStyle, Shape, Sheet, Show, Stroke, Timeline, Track,
    Transition, Triggers, FORMAT,
};
pub use output::{OutputColor, LUMA_WEIGHTS};
pub use path::{PathData, PathElement};
pub use value::Value;

#[cfg(feature = "render")]
pub mod render;

/// Re-export of the vello crate (with the `render` feature) so hosts that
/// composite the show themselves can use matching vello/wgpu types.
#[cfg(feature = "render")]
pub use vello;
