use serde::{Deserialize, Serialize};

/// Easing applied to the segment that ends at the keyframe carrying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
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
    /// Pulls back below the start before going.
    BackIn,
    /// Overshoots the end and comes back to it.
    BackOut,
    BackInOut,
    /// Arrives fast, then swings around the end a few times, settling.
    ElasticOut,
    /// Hits the end and bounces off it a few times, never passing it.
    BounceOut,
}

/// How far `back` easings overshoot: about 10% of the way.
const BACK: f64 = 1.70158;

impl Easing {
    /// Map a linear progress `t` in [0, 1] to eased progress: 0 at 0 and 1
    /// at 1, but the `back` and `elastic` kinds leave [0, 1] in between.
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
            Easing::BackIn => t * t * ((BACK + 1.0) * t - BACK),
            Easing::BackOut => {
                let u = t - 1.0;
                u * u * ((BACK + 1.0) * u + BACK) + 1.0
            }
            Easing::BackInOut => {
                let k = BACK * 1.525;
                if t < 0.5 {
                    let u = 2.0 * t;
                    0.5 * u * u * ((k + 1.0) * u - k)
                } else {
                    let u = 2.0 * t - 2.0;
                    0.5 * (u * u * ((k + 1.0) * u + k) + 2.0)
                }
            }
            Easing::ElasticOut => {
                if t == 0.0 || t == 1.0 {
                    t
                } else {
                    let turn = std::f64::consts::TAU / 3.0;
                    2f64.powf(-10.0 * t) * ((t * 10.0 - 0.75) * turn).sin() + 1.0
                }
            }
            Easing::BounceOut => {
                const N: f64 = 7.5625;
                const D: f64 = 2.75;
                if t < 1.0 / D {
                    N * t * t
                } else if t < 2.0 / D {
                    let u = t - 1.5 / D;
                    N * u * u + 0.75
                } else if t < 2.5 / D {
                    let u = t - 2.25 / D;
                    N * u * u + 0.9375
                } else {
                    let u = t - 2.625 / D;
                    N * u * u + 0.984375
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Easing::{self, *};

    const ALL: [Easing; 13] = [
        Linear, QuadIn, QuadOut, QuadInOut, CubicIn, CubicOut, CubicInOut, Step, BackIn, BackOut,
        BackInOut, ElasticOut, BounceOut,
    ];

    #[test]
    fn every_easing_starts_at_0_and_ends_at_1() {
        for ease in ALL {
            assert!(ease.apply(0.0).abs() < 1e-12, "{ease:?} at 0");
            assert!((ease.apply(1.0) - 1.0).abs() < 1e-12, "{ease:?} at 1");
            // Time outside the segment holds the ends.
            assert_eq!(ease.apply(-1.0), ease.apply(0.0));
            assert_eq!(ease.apply(2.0), ease.apply(1.0));
        }
    }

    #[test]
    fn overshooting_kinds_leave_the_range_and_bounce_does_not() {
        let range = |ease: Easing| {
            let values = (0..=200).map(|i| ease.apply(f64::from(i) / 200.0));
            values.fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
        };
        let (lo, hi) = range(BackOut);
        assert!(lo >= 0.0 && hi > 1.09 && hi < 1.11, "back_out {lo}..{hi}");
        assert!(range(BackIn).0 < -0.09);
        let (lo, hi) = range(BackInOut);
        assert!(lo < -0.05 && hi > 1.05);
        assert!(range(ElasticOut).1 > 1.2);
        let (lo, hi) = range(BounceOut);
        assert!(lo >= 0.0 && hi <= 1.0, "bounce_out {lo}..{hi}");
    }
}
