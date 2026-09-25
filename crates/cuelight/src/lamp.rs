//! An incandescent bulb's filament: how it heats, how it cools, and the
//! light it gives while it does.
//!
//! A lamp is not a fade. Switching one is the filament's temperature
//! chasing the power put into it, and what you see is a steep function of
//! that temperature: a bulb reaches full brightness in a few tens of
//! milliseconds, loses most of its light as fast when the power goes, and
//! then holds a dim red glow much longer. A bulb re-lit while still warm
//! comes up quicker than a cold one, which no fixed duration can express.
//!
//! Everything here is a function of a starting temperature, a target and
//! elapsed time, so a frame is the same however it was reached, and
//! seeking stays possible.
//!
//! # Where this comes from
//!
//! Two exponents are the standard ones for a tungsten lamp: luminous flux
//! goes as about the 3.4th power of the drive, and temperature as about
//! its 0.36th. The third, light against temperature, is not their
//! quotient: a filament sits at room temperature rather than zero when it
//! is off, and that floor stops the chain being a pure power law. It is
//! chosen instead so the whole chain reproduces the first: 11 tracks
//! flux as the 3.4th power of drive to within about a point across the
//! range, where their quotient of 9.4 is out by five.
//!
//! - Heating and cooling times: D. C. Agrawal, "Heating-times of tungsten
//!   filament incandescent lamps", European Journal of Physics 32 (2011).
//! - The colour of a filament at a temperature: the curve fit published
//!   by Tanner Helland, "How to Convert Temperature (K) to RGB", itself
//!   fitted to Mitchell Charity's blackbody colour table ("What color is
//!   a blackbody?", Harvard-Smithsonian CfA). The three constants in
//!   [`color`] are his.

use crate::model::Transition;

/// Room temperature, where a cold filament sits, in kelvin.
const AMBIENT: f64 = 300.0;

/// The temperature a filament's colour is judged against, in kelvin.
///
/// Eyes white-balance: standing next to a 2700 K lamp it looks warm
/// white, not the saturated orange a blackbody curve gives. Colours are
/// read relative to a warmer reference so a lamp at full power looks like
/// a lamp, while a cooling or dimmed one still goes visibly redder, which
/// is the part worth having. A fixed reference rather than each lamp's
/// own, so a cool little bulb and a hot big one still differ.
const REFERENCE: f64 = 3200.0;

/// Light against temperature, which is what makes a cooling bulb go dim
/// so much faster than it goes cold. See the note above on why it is
/// this and not the quotient of the other two.
const LIGHT: i32 = 11;

/// What a filament does: how hot it runs at full power, and how quickly
/// it gets there and back.
///
/// A small lamp is cooler and quicker, a big one hotter and slower, and
/// naming makes of bulb instead would only be a catalogue to learn.
#[derive(Debug, Clone, Copy)]
pub struct Filament {
    /// Temperature at full power, in kelvin, which is also the colour it
    /// glows there.
    pub full: f64,
    /// Seconds to close about two thirds of the gap, heating.
    pub heating: f64,
    /// The same, cooling. Always the slower of the two: heat leaves more
    /// slowly than power puts it in.
    pub cooling: f64,
}

impl Default for Filament {
    fn default() -> Self {
        Filament {
            full: 2700.0,
            heating: 0.007,
            cooling: 0.060,
        }
    }
}

impl Filament {
    /// What a transition asks for, with the defaults where it says
    /// nothing.
    pub fn of(transition: &Transition) -> Filament {
        let sane = |n: Option<f64>, fallback: f64| match n {
            Some(n) if n.is_finite() && n > 0.0 => n,
            _ => fallback,
        };
        let default = Filament::default();
        Filament {
            full: sane(transition.kelvin, default.full),
            heating: sane(transition.heating, default.heating),
            cooling: sane(transition.cooling, default.cooling),
        }
    }
}

/// The temperature a filament settles at under `power` (0 to 1).
///
/// `power` is the drive, as a fraction of full: what a dimmer or a duty
/// cycle sets, not a brightness already worked out. Temperature goes as
/// about its 0.36th power, so half drive is much more than half the
/// temperature; that is why a lamp dimmed a little barely changes colour
/// and one dimmed a lot goes red.
pub fn settled(lamp: Filament, power: f64) -> f64 {
    AMBIENT + (lamp.full - AMBIENT) * power.clamp(0.0, 1.0).powf(0.36)
}

/// The temperature `elapsed` seconds after it was `from`, heading for the
/// temperature `power` settles at.
pub fn temperature(lamp: Filament, from: f64, power: f64, elapsed: f64) -> f64 {
    let target = settled(lamp, power);
    if elapsed <= 0.0 {
        return from;
    }
    let tau = if target >= from {
        lamp.heating
    } else {
        lamp.cooling
    };
    target + (from - target) * (-elapsed / tau).exp()
}

/// How bright a filament at `temperature` is, 0 to 1 against full power.
pub fn brightness(lamp: Filament, temperature: f64) -> f64 {
    let full = lamp.full;
    if temperature <= AMBIENT || full <= AMBIENT {
        return 0.0;
    }
    (temperature / full).clamp(0.0, 1.0).powi(LIGHT)
}

/// The light a filament at `temperature` shows as, 0 to 1.
///
/// [`brightness`] is physical light, which is linear; an `opacity` is
/// display-referred, and blending treats it that way. Without the
/// transfer between them everything dim reads as black: the dim red glow
/// a filament holds for tens of milliseconds after the power goes never
/// appears, and half drive looks like a dull brown rather than a lamp
/// turned down.
pub fn shown(lamp: Filament, temperature: f64) -> f64 {
    let light = brightness(lamp, temperature);
    // The sRGB transfer, the one a display already assumes.
    if light <= 0.003_130_8 {
        12.92 * light
    } else {
        1.055 * light.powf(1.0 / 2.4) - 0.055
    }
}

/// The colour of a filament at `temperature`, as RGB.
///
/// A blackbody at that temperature, read against [`REFERENCE`] so a lamp
/// at full power is a warm white rather than an orange: how dim it is
/// belongs to the brightness, and how red it is belongs here.
///
/// The constants are Tanner Helland's curve fit to Mitchell Charity's
/// blackbody colour table; see the note at the top of this module.
pub fn color(temperature: f64) -> [u8; 3] {
    let raw = blackbody(temperature);
    let reference = blackbody(REFERENCE);
    let against = |c: f64, r: f64| {
        if r <= 0.0 {
            0
        } else {
            (255.0 * c / r).clamp(0.0, 255.0) as u8
        }
    };
    [
        against(raw[0], reference[0]),
        against(raw[1], reference[1]),
        against(raw[2], reference[2]),
    ]
}

/// The raw blackbody colour at `temperature`, before any reference.
fn blackbody(temperature: f64) -> [f64; 3] {
    // Kelvin in hundreds, the form the usual approximation is written in.
    let t = (temperature.clamp(1000.0, 40000.0)) / 100.0;
    let clamp = |v: f64| v.clamp(0.0, 255.0);
    let red = if t <= 66.0 {
        255.0
    } else {
        329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2)
    };
    let green = if t <= 66.0 {
        99.470_802_586 * t.ln() - 161.119_568_166
    } else {
        288.122_169_528 * (t - 60.0).powf(-0.075_514_849_2)
    };
    let blue = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_731_223 * (t - 10.0).ln() - 305.044_792_7
    };
    [clamp(red), clamp(green), clamp(blue)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cold_bulb_reaches_full_brightness_in_a_few_tens_of_milliseconds() {
        let cold = AMBIENT;
        let at = |ms: f64| {
            brightness(
                Filament::default(),
                temperature(Filament::default(), cold, 1.0, ms / 1000.0),
            )
        };
        assert!(at(0.0) < 0.001, "{}", at(0.0));
        assert!(at(10.0) < 0.5, "still coming up at 10 ms: {}", at(10.0));
        assert!(at(35.0) > 0.85, "much of the way by 35 ms: {}", at(35.0));
        assert!(at(60.0) > 0.98, "there by 60 ms: {}", at(60.0));
    }

    #[test]
    fn it_loses_its_light_faster_than_its_heat() {
        let hot = settled(Filament::default(), 1.0);
        let dark = |ms: f64| {
            brightness(
                Filament::default(),
                temperature(Filament::default(), hot, 0.0, ms / 1000.0),
            )
        };
        let warm = |ms: f64| temperature(Filament::default(), hot, 0.0, ms / 1000.0);
        assert!(dark(30.0) < 0.1, "dim fast: {}", dark(30.0));
        assert!(
            warm(30.0) > 1500.0,
            "but still hot enough to glow: {}",
            warm(30.0)
        );
        assert!(dark(400.0) < 0.001, "and out in the end: {}", dark(400.0));
    }

    #[test]
    fn a_warm_bulb_comes_up_quicker_than_a_cold_one() {
        let cold = brightness(
            Filament::default(),
            temperature(Filament::default(), AMBIENT, 1.0, 0.010),
        );
        let warm = brightness(
            Filament::default(),
            temperature(Filament::default(), 2000.0, 1.0, 0.010),
        );
        assert!(warm > cold * 2.0, "cold {cold}, warm {warm}");
    }

    #[test]
    fn a_lamp_at_full_power_is_a_warm_white_not_an_orange() {
        // The raw blackbody curve puts 2700 K at a saturated orange. Read
        // against the reference it is a warm white, which is what one
        // looks like standing next to it.
        let [r, g, b] = color(2700.0);
        assert_eq!(r, 255);
        assert!(g > 220, "green {g}");
        assert!(b > 160, "blue {b}");
    }

    #[test]
    fn a_cooler_lamp_stays_warmer_than_a_hotter_one() {
        // Each lamp is read against the same reference, so a small cool
        // bulb and a big hot one still differ at full power.
        let cool = color(2300.0);
        let hot = color(2900.0);
        assert!(cool[2] < hot[2], "cool {cool:?} hot {hot:?}");
    }

    #[test]
    fn a_dim_filament_shows_more_than_its_light() {
        // Linear light straight into an opacity reads as black; through
        // the display transfer, the afterglow is visible.
        let lamp = Filament::default();
        let cooling = temperature(lamp, settled(lamp, 1.0), 0.0, 0.02);
        let light = brightness(lamp, cooling);
        let seen = shown(lamp, cooling);
        assert!(light < 0.05, "light {light}");
        assert!(seen > 3.0 * light, "light {light} shown as {seen}");
    }

    #[test]
    fn a_dim_filament_is_redder_than_a_bright_one() {
        let [hot_r, _, hot_b] = color(settled(Filament::default(), 1.0));
        let [dim_r, _, dim_b] = color(1400.0);
        assert_eq!((hot_r, dim_r), (255, 255), "red is pinned at these");
        assert!(dim_b < hot_b, "the blue goes first: {dim_b} vs {hot_b}");
    }

    #[test]
    fn a_quick_filament_is_as_good_as_instant() {
        let quick = Filament {
            heating: 0.0005,
            cooling: 0.0005,
            ..Filament::default()
        };
        let at = brightness(quick, temperature(quick, AMBIENT, 1.0, 0.005));
        assert!(at > 0.99, "{at}");
    }

    #[test]
    fn a_hotter_filament_glows_whiter() {
        let small = Filament::default();
        let big = Filament {
            full: 2900.0,
            ..Filament::default()
        };
        let [_, _, small_b] = color(settled(small, 1.0));
        let [_, _, big_b] = color(settled(big, 1.0));
        assert!(big_b > small_b, "{big_b} vs {small_b}");
    }
}
