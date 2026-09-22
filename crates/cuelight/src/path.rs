//! Path geometry: SVG path data parsed into lines and curves.

use serde::{Deserialize, Serialize};

/// One step of a path, in the coordinates of whatever holds the path.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum PathElement {
    MoveTo([f64; 2]),
    LineTo([f64; 2]),
    /// A quadratic curve: control point, then end point.
    QuadTo([f64; 2], [f64; 2]),
    /// A cubic curve: two control points, then the end point.
    CubicTo([f64; 2], [f64; 2], [f64; 2]),
    /// Back to the start of the current subpath.
    Close,
}

impl PathElement {
    /// The element with `f` applied to every point.
    pub fn map(self, mut f: impl FnMut([f64; 2]) -> [f64; 2]) -> PathElement {
        match self {
            PathElement::MoveTo(p) => PathElement::MoveTo(f(p)),
            PathElement::LineTo(p) => PathElement::LineTo(f(p)),
            PathElement::QuadTo(c, p) => PathElement::QuadTo(f(c), f(p)),
            PathElement::CubicTo(c1, c2, p) => PathElement::CubicTo(f(c1), f(c2), f(p)),
            PathElement::Close => PathElement::Close,
        }
    }

    /// Every point of the element, control points included.
    pub fn points(&self) -> impl Iterator<Item = [f64; 2]> {
        let points: [Option<[f64; 2]>; 3] = match *self {
            PathElement::MoveTo(p) | PathElement::LineTo(p) => [Some(p), None, None],
            PathElement::QuadTo(c, p) => [Some(c), Some(p), None],
            PathElement::CubicTo(c1, c2, p) => [Some(c1), Some(c2), Some(p)],
            PathElement::Close => [None; 3],
        };
        points.into_iter().flatten()
    }
}

/// The bounding box `[x, y, width, height]` of a path's points, control
/// points included; `None` for a path without points.
pub fn bounds(elements: &[PathElement]) -> Option<[f64; 4]> {
    let mut points = elements.iter().flat_map(PathElement::points);
    let [x0, y0] = points.next()?;
    let (mut min, mut max) = ([x0, y0], [x0, y0]);
    for [x, y] in points {
        min = [min[0].min(x), min[1].min(y)];
        max = [max[0].max(x), max[1].max(y)];
    }
    Some([min[0], min[1], max[0] - min[0], max[1] - min[1]])
}

/// Path data as a show authors it: an SVG path string (`M 0 0 L 10 0 ...`),
/// parsed once into [`PathElement`]s. Serializes as the string it was
/// written as.
#[derive(Debug, Clone, PartialEq)]
pub struct PathData {
    d: String,
    elements: Vec<PathElement>,
}

impl PathData {
    /// Parse SVG path data: the commands `M L H V C S Q T A Z`, absolute
    /// and relative, with numbers separated by spaces or commas. Arcs
    /// (`A`) become cubic curves.
    pub fn parse(d: &str) -> Result<PathData, String> {
        let elements = Parser::new(d).run()?;
        Ok(PathData {
            d: d.to_owned(),
            elements,
        })
    }

    /// The path string as written.
    pub fn as_str(&self) -> &str {
        &self.d
    }

    pub fn elements(&self) -> &[PathElement] {
        &self.elements
    }
}

impl Serialize for PathData {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.d)
    }
}

impl<'de> Deserialize<'de> for PathData {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let d = String::deserialize(deserializer)?;
        PathData::parse(&d).map_err(|e| serde::de::Error::custom(format!("path data: {e}")))
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for PathData {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PathData".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "SVG path data: M L H V C S Q T A Z commands, absolute or relative.",
            "type": "string"
        })
    }
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    out: Vec<PathElement>,
    /// Current point.
    at: [f64; 2],
    /// Start of the current subpath, where `Z` returns to.
    start: [f64; 2],
    /// Last control point, for the smooth curve commands.
    last_cubic: Option<[f64; 2]>,
    last_quad: Option<[f64; 2]>,
}

impl<'a> Parser<'a> {
    fn new(d: &'a str) -> Self {
        Self {
            src: d.as_bytes(),
            pos: 0,
            out: Vec::new(),
            at: [0.0; 2],
            start: [0.0; 2],
            last_cubic: None,
            last_quad: None,
        }
    }

    fn skip_separators(&mut self) {
        while let Some(&c) = self.src.get(self.pos) {
            if c.is_ascii_whitespace() || c == b',' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Whether a number starts here.
    fn at_number(&mut self) -> bool {
        self.skip_separators();
        matches!(self.src.get(self.pos), Some(c) if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'))
    }

    fn number(&mut self) -> Result<f64, String> {
        self.skip_separators();
        let start = self.pos;
        let mut seen_digit = false;
        let mut seen_dot = false;
        let mut seen_exp = false;
        if matches!(self.src.get(self.pos), Some(b'-' | b'+')) {
            self.pos += 1;
        }
        while let Some(&c) = self.src.get(self.pos) {
            match c {
                b'0'..=b'9' => seen_digit = true,
                b'.' if !seen_dot && !seen_exp => seen_dot = true,
                b'e' | b'E' if seen_digit && !seen_exp => {
                    seen_exp = true;
                    if matches!(self.src.get(self.pos + 1), Some(b'-' | b'+')) {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        text.parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .ok_or_else(|| format!("expected a number at byte {start}"))
    }

    /// An arc flag: a single `0` or `1`, which may run into the next
    /// number without a separator.
    fn flag(&mut self) -> Result<bool, String> {
        self.skip_separators();
        let at = self.pos;
        match self.src.get(self.pos) {
            Some(b'0') => {
                self.pos += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.pos += 1;
                Ok(true)
            }
            _ => Err(format!("expected an arc flag (0 or 1) at byte {at}")),
        }
    }

    fn point(&mut self, relative: bool) -> Result<[f64; 2], String> {
        let x = self.number()?;
        let y = self.number()?;
        Ok(if relative {
            [self.at[0] + x, self.at[1] + y]
        } else {
            [x, y]
        })
    }

    fn run(mut self) -> Result<Vec<PathElement>, String> {
        let mut command: Option<u8> = None;
        loop {
            self.skip_separators();
            let Some(&c) = self.src.get(self.pos) else {
                break;
            };
            if c.is_ascii_alphabetic() {
                self.pos += 1;
                command = Some(c);
            } else if command.is_none() || matches!(command, Some(b'Z' | b'z')) {
                return Err(format!("expected a command at byte {}", self.pos));
            } else if !self.at_number() {
                return Err(format!("unexpected {:?} at byte {}", c as char, self.pos));
            }
            let cmd = command.ok_or("path data must start with a command")?;
            if self.out.is_empty() && !matches!(cmd, b'M' | b'm') {
                return Err("path data must start with M".into());
            }
            let relative = cmd.is_ascii_lowercase();
            match cmd.to_ascii_uppercase() {
                b'M' => {
                    let p = self.point(relative)?;
                    self.out.push(PathElement::MoveTo(p));
                    self.at = p;
                    self.start = p;
                    // Further pairs after a move are lines.
                    command = Some(if relative { b'l' } else { b'L' });
                    self.reset_controls();
                }
                b'L' => {
                    let p = self.point(relative)?;
                    self.line_to(p);
                }
                b'H' => {
                    let x = self.number()?;
                    let x = if relative { self.at[0] + x } else { x };
                    self.line_to([x, self.at[1]]);
                }
                b'V' => {
                    let y = self.number()?;
                    let y = if relative { self.at[1] + y } else { y };
                    self.line_to([self.at[0], y]);
                }
                b'C' => {
                    let c1 = self.point(relative)?;
                    let c2 = self.point(relative)?;
                    let p = self.point(relative)?;
                    self.cubic_to(c1, c2, p);
                }
                b'S' => {
                    let c1 = self.reflect(self.last_cubic);
                    let c2 = self.point(relative)?;
                    let p = self.point(relative)?;
                    self.cubic_to(c1, c2, p);
                }
                b'Q' => {
                    let c = self.point(relative)?;
                    let p = self.point(relative)?;
                    self.quad_to(c, p);
                }
                b'T' => {
                    let c = self.reflect(self.last_quad);
                    let p = self.point(relative)?;
                    self.quad_to(c, p);
                }
                b'A' => {
                    let rx = self.number()?;
                    let ry = self.number()?;
                    let rotation = self.number()?;
                    let large = self.flag()?;
                    let sweep = self.flag()?;
                    let p = self.point(relative)?;
                    self.arc_to(rx, ry, rotation, large, sweep, p);
                }
                b'Z' => {
                    self.out.push(PathElement::Close);
                    self.at = self.start;
                    self.reset_controls();
                }
                other => return Err(format!("unknown path command {:?}", other as char)),
            }
        }
        if self.out.is_empty() {
            return Err("path data is empty".into());
        }
        Ok(self.out)
    }

    fn reset_controls(&mut self) {
        self.last_cubic = None;
        self.last_quad = None;
    }

    /// The last control point mirrored through the current point, or the
    /// current point when the previous command was not of that kind.
    fn reflect(&self, last: Option<[f64; 2]>) -> [f64; 2] {
        match last {
            Some([cx, cy]) => [2.0 * self.at[0] - cx, 2.0 * self.at[1] - cy],
            None => self.at,
        }
    }

    fn line_to(&mut self, p: [f64; 2]) {
        self.out.push(PathElement::LineTo(p));
        self.at = p;
        self.reset_controls();
    }

    fn cubic_to(&mut self, c1: [f64; 2], c2: [f64; 2], p: [f64; 2]) {
        self.out.push(PathElement::CubicTo(c1, c2, p));
        self.at = p;
        self.last_cubic = Some(c2);
        self.last_quad = None;
    }

    fn quad_to(&mut self, c: [f64; 2], p: [f64; 2]) {
        self.out.push(PathElement::QuadTo(c, p));
        self.at = p;
        self.last_quad = Some(c);
        self.last_cubic = None;
    }

    /// An elliptical arc as one cubic per quarter turn or less, following
    /// the SVG implementation notes (endpoint to center conversion).
    fn arc_to(&mut self, rx: f64, ry: f64, rotation: f64, large: bool, sweep: bool, p: [f64; 2]) {
        let [x1, y1] = self.at;
        let [x2, y2] = p;
        if (x1 == x2 && y1 == y2) || rx == 0.0 || ry == 0.0 {
            self.line_to(p);
            return;
        }
        let (mut rx, mut ry) = (rx.abs(), ry.abs());
        let (sin, cos) = rotation.to_radians().sin_cos();
        // Step 1: the midpoint of the chord in the ellipse's own frame.
        let dx = (x1 - x2) / 2.0;
        let dy = (y1 - y2) / 2.0;
        let x1p = cos * dx + sin * dy;
        let y1p = -sin * dx + cos * dy;
        // Radii too small for the chord are scaled up.
        let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
        if lambda > 1.0 {
            rx *= lambda.sqrt();
            ry *= lambda.sqrt();
        }
        // Step 2: the center in that frame.
        let num = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
        let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
        let mut coef = if den == 0.0 { 0.0 } else { (num / den).sqrt() };
        if large == sweep {
            coef = -coef;
        }
        let cxp = coef * rx * y1p / ry;
        let cyp = -coef * ry * x1p / rx;
        // Step 3: back to the canvas frame.
        let cx = cos * cxp - sin * cyp + (x1 + x2) / 2.0;
        let cy = sin * cxp + cos * cyp + (y1 + y2) / 2.0;
        // Step 4: the start angle and sweep.
        let angle = |ux: f64, uy: f64, vx: f64, vy: f64| {
            let dot = ux * vx + uy * vy;
            let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
            let mut a = (dot / len).clamp(-1.0, 1.0).acos();
            if ux * vy - uy * vx < 0.0 {
                a = -a;
            }
            a
        };
        let theta1 = angle(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
        let mut delta = angle(
            (x1p - cxp) / rx,
            (y1p - cyp) / ry,
            (-x1p - cxp) / rx,
            (-y1p - cyp) / ry,
        );
        if !sweep && delta > 0.0 {
            delta -= std::f64::consts::TAU;
        } else if sweep && delta < 0.0 {
            delta += std::f64::consts::TAU;
        }
        // Cubics of at most a quarter turn each.
        let segments = (delta.abs() / std::f64::consts::FRAC_PI_2).ceil().max(1.0) as usize;
        let step = delta / segments as f64;
        let t = (step / 2.0).tan();
        let alpha = (step.sin() * ((4.0 + 3.0 * t * t).sqrt() - 1.0)) / 3.0;
        let point = |a: f64| {
            let (sa, ca) = a.sin_cos();
            [
                cx + rx * ca * cos - ry * sa * sin,
                cy + rx * ca * sin + ry * sa * cos,
            ]
        };
        let tangent = |a: f64| {
            let (sa, ca) = a.sin_cos();
            [
                -rx * sa * cos - ry * ca * sin,
                -rx * sa * sin + ry * ca * cos,
            ]
        };
        let mut a = theta1;
        for i in 0..segments {
            let b = a + step;
            let p0 = point(a);
            let p3 = if i + 1 == segments { p } else { point(b) };
            let t0 = tangent(a);
            let t3 = tangent(b);
            let c1 = [p0[0] + alpha * t0[0], p0[1] + alpha * t0[1]];
            let c2 = [p3[0] - alpha * t3[0], p3[1] - alpha * t3[1]];
            self.out.push(PathElement::CubicTo(c1, c2, p3));
            a = b;
        }
        self.at = p;
        self.reset_controls();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_and_relative_commands() {
        let path = PathData::parse("M10,10 l20 0 v10 H10 z").unwrap();
        assert_eq!(
            path.elements(),
            &[
                PathElement::MoveTo([10.0, 10.0]),
                PathElement::LineTo([30.0, 10.0]),
                PathElement::LineTo([30.0, 20.0]),
                PathElement::LineTo([10.0, 20.0]),
                PathElement::Close,
            ]
        );
        assert_eq!(bounds(path.elements()), Some([10.0, 10.0, 20.0, 10.0]));
    }

    #[test]
    fn implicit_lines_after_move_and_smooth_curves() {
        let path = PathData::parse("M0 0 10 0 10 10 C 10 20 0 20 0 10 S -10 0 0 0").unwrap();
        assert_eq!(path.elements()[1], PathElement::LineTo([10.0, 0.0]));
        assert_eq!(path.elements()[2], PathElement::LineTo([10.0, 10.0]));
        assert_eq!(
            path.elements()[4],
            PathElement::CubicTo([0.0, 0.0], [-10.0, 0.0], [0.0, 0.0])
        );
        let quad = PathData::parse("M0 0 Q 5 10 10 0 T 20 0").unwrap();
        assert_eq!(
            quad.elements()[2],
            PathElement::QuadTo([15.0, -10.0], [20.0, 0.0])
        );
    }

    #[test]
    fn arcs_become_cubics_that_end_on_the_point() {
        let path = PathData::parse("M0 0 A 10 10 0 0 1 20 0").unwrap();
        // A half circle: two quarter-turn cubics.
        assert_eq!(path.elements().len(), 3);
        match path.elements()[2] {
            PathElement::CubicTo(_, _, p) => assert_eq!(p, [20.0, 0.0]),
            other => panic!("{other:?}"),
        }
        // The middle lands on the circle's top: sweep flag 1 goes
        // clockwise as seen on a y-down canvas, over the chord.
        match path.elements()[1] {
            PathElement::CubicTo(_, _, [x, y]) => {
                assert!(
                    (x - 10.0).abs() < 1e-9 && (y + 10.0).abs() < 1e-9,
                    "{x} {y}"
                );
            }
            other => panic!("{other:?}"),
        }
        // Flags run together, as authoring tools write them.
        assert!(PathData::parse("M0 0a10 10 0 0120 0").is_ok());
    }

    #[test]
    fn rejects_bad_data() {
        assert!(PathData::parse("").is_err());
        assert!(PathData::parse("L 10 10").is_err());
        assert!(PathData::parse("M 10").is_err());
        assert!(PathData::parse("M 10 10 X 5").is_err());
        assert!(PathData::parse("M 10 10 L 1 2 3").is_err());
        assert!(PathData::parse("M 1e 2").is_err());
    }
}
