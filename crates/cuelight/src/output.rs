use crate::model::{parse_color, Output, OutputMode};

/// Weights `[r, g, b]` for luma: how bright a color looks, as one number.
///
/// The eye is far more sensitive to green than to red, and least to blue,
/// so a plain average would make pure blue look as bright as pure green.
/// These weights come from Rec. 709 (the HDTV standard that also defines
/// the sRGB primaries): `luma = 0.2126 r + 0.7152 g + 0.0722 b`. They are
/// applied directly to the encoded 8-bit channel values, without
/// linearizing first, so pure green lands at about 72 % brightness, pure
/// red at 21 % and pure blue at 7 %.
pub const LUMA_WEIGHTS: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// Resolved output color handling for the current frame: what
/// [`Engine::output`](crate::Engine::output) returns and what renderers
/// apply to the finished frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputColor {
    pub mode: OutputMode,
    /// RGB that full luminance maps to in the gray modes.
    pub tint: [u8; 3],
}

impl OutputColor {
    /// Full color, frames pass through unchanged.
    pub const RGB: OutputColor = OutputColor {
        mode: OutputMode::Rgb,
        tint: [255, 255, 255],
    };

    /// Resolve a show document's output description; `None` when the tint
    /// is not a valid color.
    pub fn from_output(output: &Output) -> Option<Self> {
        let tint = match &output.tint {
            Some(tint) => {
                let [r, g, b, _] = parse_color(tint)?;
                [r, g, b]
            }
            None => [255, 255, 255],
        };
        Some(match output.mode.unwrap_or_default() {
            // The tint plays no part in full color.
            OutputMode::Rgb => Self::RGB,
            mode => Self { mode, tint },
        })
    }

    /// Highest luminance level of the gray modes (levels run 0..=max), or
    /// `None` in full-color mode.
    pub fn max_level(&self) -> Option<u8> {
        match self.mode {
            OutputMode::Rgb => None,
            OutputMode::Gray2 => Some(3),
            OutputMode::Gray4 => Some(15),
        }
    }

    /// The quantized luminance level (0..=max) of an RGB color, or `None`
    /// in full-color mode. Useful to hosts that drive gray-level hardware.
    pub fn level(&self, rgb: [u8; 3]) -> Option<u8> {
        let max = f64::from(self.max_level()?);
        let lum = rgb
            .iter()
            .zip(LUMA_WEIGHTS)
            .map(|(&c, w)| f64::from(c) * w)
            .sum::<f64>()
            / 255.0;
        Some((lum * max).round() as u8)
    }

    /// Convert one RGBA pixel; alpha is kept.
    pub fn apply_pixel(&self, [r, g, b, a]: [u8; 4]) -> [u8; 4] {
        let (Some(level), Some(max)) = (self.level([r, g, b]), self.max_level()) else {
            return [r, g, b, a];
        };
        let scale = |c: u8| (f64::from(c) * f64::from(level) / f64::from(max)).round() as u8;
        let [tr, tg, tb] = self.tint;
        [scale(tr), scale(tg), scale(tb), a]
    }

    /// Convert tightly packed RGBA8 pixels in place.
    pub fn apply(&self, rgba: &mut [u8]) {
        if self.mode == OutputMode::Rgb {
            return;
        }
        for pixel in rgba.as_chunks_mut::<4>().0 {
            *pixel = self.apply_pixel(*pixel);
        }
    }
}

impl Default for OutputColor {
    fn default() -> Self {
        Self::RGB
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray4(tint: [u8; 3]) -> OutputColor {
        OutputColor {
            mode: OutputMode::Gray4,
            tint,
        }
    }

    #[test]
    fn rgb_passes_through() {
        let mut px = [10, 20, 30, 40];
        OutputColor::RGB.apply(&mut px);
        assert_eq!(px, [10, 20, 30, 40]);
    }

    #[test]
    fn gray4_quantizes_luminance_and_tints() {
        let orange = gray4([255, 88, 32]);
        assert_eq!(orange.apply_pixel([255, 255, 255, 255]), [255, 88, 32, 255]);
        assert_eq!(orange.apply_pixel([0, 0, 0, 255]), [0, 0, 0, 255]);
        // #404040 has luminance 64/255 -> level round(3.76) = 4 of 15.
        assert_eq!(orange.level([64, 64, 64]), Some(4));
        assert_eq!(orange.apply_pixel([64, 64, 64, 255]), [68, 23, 9, 255]);
        // Pure green is bright, pure blue dark.
        assert_eq!(orange.level([0, 255, 0]), Some(11));
        assert_eq!(orange.level([0, 0, 255]), Some(1));
    }

    #[test]
    fn gray2_has_four_levels() {
        let white = OutputColor {
            mode: OutputMode::Gray2,
            tint: [255, 255, 255],
        };
        assert_eq!(
            white.apply_pixel([128, 128, 128, 255]),
            [170, 170, 170, 255]
        );
    }

    #[test]
    fn tint_parses_and_defaults_to_white() {
        let output = Output {
            mode: Some(OutputMode::Gray4),
            tint: Some("#FF5820".into()),
            scaling: None,
        };
        assert_eq!(
            OutputColor::from_output(&output),
            Some(gray4([255, 88, 32]))
        );
        let output = Output {
            mode: Some(OutputMode::Gray4),
            tint: None,
            scaling: None,
        };
        assert_eq!(
            OutputColor::from_output(&output),
            Some(gray4([255, 255, 255]))
        );
        let output = Output {
            mode: Some(OutputMode::Gray4),
            tint: Some("orange".into()),
            scaling: None,
        };
        assert_eq!(OutputColor::from_output(&output), None);
    }
}
