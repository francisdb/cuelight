use serde::{Deserialize, Serialize};

/// Easing applied to the segment that ends at the keyframe carrying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Easing {
    #[default]
    Linear,
    QuadIn,
    QuadOut,
    QuadInOut,
    CubicIn,
    CubicOut,
    CubicInOut,
    Step,
}

impl Easing {
    /// Map a linear progress `t` in [0, 1] to eased progress.
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::QuadIn => t * t,
            Easing::QuadOut => t * (2.0 - t),
            Easing::QuadInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    -1.0 + (4.0 - 2.0 * t) * t
                }
            }
            Easing::CubicIn => t * t * t,
            Easing::CubicOut => {
                let u = t - 1.0;
                u * u * u + 1.0
            }
            Easing::CubicInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let u = 2.0 * t - 2.0;
                    0.5 * u * u * u + 1.0
                }
            }
            Easing::Step => {
                if t < 1.0 {
                    0.0
                } else {
                    1.0
                }
            }
        }
    }
}
