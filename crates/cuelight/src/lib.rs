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
//! plus obtaining the rendered output: with the `render` feature enabled,
//! [`render::Renderer`] rasterizes the engine's resolved show into an
//! offscreen wgpu texture via vello and can read it back as RGBA pixels.
//!
//! Everything else belongs to hosts and adapters, not this crate.

mod easing;
mod engine;
mod font;
mod model;
mod value;

pub use easing::Easing;
pub use engine::{Engine, Error, ImageData, ResolvedLayer, ResolvedShape};
pub use font::BitmapFont;
pub use model::{
    Align, Binding, Border, FontStyle, Key, Layer, LayerKind, NumberFormat, Property, Scene, Shape,
    Show, Timeline, Track,
};
pub use value::Value;

#[cfg(feature = "render")]
pub mod render;

/// Re-export of the vello crate (with the `render` feature) so hosts that
/// composite the show themselves can use matching vello/wgpu types.
#[cfg(feature = "render")]
pub use vello;
