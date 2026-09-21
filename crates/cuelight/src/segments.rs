//! Segment display geometry: which segments light up for a character and
//! where each segment sits in a digit cell.

use crate::model::{Justify, SegmentStyle};

/// 14-segment character masks, one bit per segment:
/// 0 a (top), 1 b (upper right), 2 c (lower right), 3 d (bottom),
/// 4 e (lower left), 5 f (upper left), 6 g1 (middle left), 7 dot,
/// 8 h (upper left diagonal), 9 j (upper center), 10 k (upper right
/// diagonal), 11 g2 (middle right), 12 l (lower right diagonal),
/// 13 m (lower center), 14 n (lower left diagonal).
fn alpha14_mask(c: char) -> u16 {
    match c.to_ascii_uppercase() {
        '0' => 0x443F,
        '1' => 0x0406,
        '2' => 0x085B,
        '3' => 0x080F,
        '4' => 0x0866,
        '5' => 0x086D,
        '6' => 0x087D,
        '7' => 0x2401,
        '8' => 0x087F,
        '9' => 0x086F,
        'A' => 0x0877,
        'B' => 0x2A0F,
        'C' => 0x0039,
        'D' => 0x220F,
        'E' => 0x0879,
        'F' => 0x0871,
        'G' => 0x083D,
        'H' => 0x0876,
        'I' => 0x2209,
        'J' => 0x001E,
        'K' => 0x1470,
        'L' => 0x0038,
        'M' => 0x0536,
        'N' => 0x1136,
        'O' => 0x003F,
        'P' => 0x0873,
        'Q' => 0x103F,
        'R' => 0x1873,
        'S' => 0x090D,
        'T' => 0x2201,
        'U' => 0x003E,
        'V' => 0x4430,
        'W' => 0x5036,
        'X' => 0x5500,
        'Y' => 0x2500,
        'Z' => 0x4409,
        '-' => 0x0840,
        '+' => 0x2A40,
        '*' => 0x7F40,
        '/' => 0x4400,
        '\\' => 0x1100,
        '=' => 0x0848,
        '_' => 0x0008,
        '\'' => 0x0200,
        '.' | ',' => 0x0080,
        _ => 0,
    }
}

/// 7-segment masks: bits 0-6 are a-g (g the middle bar), 7 the dot.
fn numeric7_mask(c: char) -> u16 {
    match c {
        '0' => 0x3F,
        '1' => 0x06,
        '2' => 0x5B,
        '3' => 0x4F,
        '4' => 0x66,
        '5' => 0x6D,
        '6' => 0x7D,
        '7' => 0x07,
        '8' => 0x7F,
        '9' => 0x6F,
        '-' => 0x40,
        '_' => 0x08,
        '.' | ',' => 0x80,
        _ => 0,
    }
}

/// The segment mask for one character of a display in `style`.
pub fn mask(style: SegmentStyle, c: char) -> u16 {
    match style {
        SegmentStyle::Alpha14 => alpha14_mask(c),
        SegmentStyle::Numeric7 => numeric7_mask(c),
    }
}

const DOT: u16 = 0x80;

/// The segment mask of each of `digits` cells showing `text`. A `.` or `,`
/// lights the dot of the cell before it rather than taking a cell of its
/// own (unless there is none, or its dot is already lit). Text that does
/// not fit is cut at the far side of `justify`.
pub fn masks(style: SegmentStyle, text: &str, digits: usize, justify: Justify) -> Vec<u16> {
    let mut cells: Vec<u16> = Vec::new();
    for c in text.chars() {
        let m = mask(style, c);
        match cells.last_mut() {
            Some(last) if m == DOT && *last & DOT == 0 => *last |= DOT,
            _ => cells.push(m),
        }
    }
    match justify {
        Justify::Right => {
            let skip = cells.len().saturating_sub(digits);
            let mut out = vec![0; digits.saturating_sub(cells.len())];
            out.extend(cells.into_iter().skip(skip));
            out
        }
        _ => {
            cells.resize(digits, 0);
            cells
        }
    }
}

/// Polygons of the lit segments of one digit cell `[x, y, w, h]`.
///
/// With `snap` the bars are a whole number of pixels thick and their long
/// edges lie on pixel boundaries. On a small canvas that decides how a
/// display looks: a one pixel bar centered on a boundary (the middle bar of
/// a cell with an even height) would otherwise be smeared over two rows at
/// half brightness.
pub fn polygons(
    style: SegmentStyle,
    mask: u16,
    [x, y, w, h]: [f64; 4],
    snap: bool,
) -> Vec<Vec<[f64; 2]>> {
    let pad = w * 0.12;
    let mut thickness = w * 0.1;
    if snap {
        thickness = thickness.round().max(1.0);
    }
    // A centerline whose bar has its edges on pixel boundaries.
    let on_grid = |c: f64| match snap {
        true => (c - thickness / 2.0).round() + thickness / 2.0,
        false => c,
    };
    let (x0, x1, xm) = (on_grid(x + pad), on_grid(x + w - pad), on_grid(x + w / 2.0));
    let (y0, y1, ym) = (on_grid(y + pad), on_grid(y + h - pad), on_grid(y + h / 2.0));
    // Segment centerlines by bit.
    let lines: &[(u16, [f64; 2], [f64; 2])] = match style {
        SegmentStyle::Alpha14 => &[
            (0, [x0, y0], [x1, y0]),
            (1, [x1, y0], [x1, ym]),
            (2, [x1, ym], [x1, y1]),
            (3, [x0, y1], [x1, y1]),
            (4, [x0, ym], [x0, y1]),
            (5, [x0, y0], [x0, ym]),
            (6, [x0, ym], [xm, ym]),
            (8, [x0, y0], [xm, ym]),
            (9, [xm, y0], [xm, ym]),
            (10, [x1, y0], [xm, ym]),
            (11, [xm, ym], [x1, ym]),
            (12, [xm, ym], [x1, y1]),
            (13, [xm, ym], [xm, y1]),
            (14, [xm, ym], [x0, y1]),
        ],
        SegmentStyle::Numeric7 => &[
            (0, [x0, y0], [x1, y0]),
            (1, [x1, y0], [x1, ym]),
            (2, [x1, ym], [x1, y1]),
            (3, [x0, y1], [x1, y1]),
            (4, [x0, ym], [x0, y1]),
            (5, [x0, y0], [x0, ym]),
            (6, [x0, ym], [x1, ym]),
        ],
    };
    let mut out: Vec<Vec<[f64; 2]>> = lines
        .iter()
        .filter(|(bit, _, _)| mask & (1 << bit) != 0)
        .map(|&(_, from, to)| bar(from, to, thickness, snap))
        .collect();
    if mask & DOT != 0 {
        // The dot sits in the bottom-right padding.
        let (cx, cy, r) = (on_grid(x + w - pad / 2.0), y1, thickness / 2.0);
        out.push(vec![
            [cx - r, cy - r],
            [cx + r, cy - r],
            [cx + r, cy + r],
            [cx - r, cy + r],
        ]);
    }
    out
}

/// A segment of `thickness` along `from`-`to`, ends shortened and pointed
/// so touching segments stay visibly apart. With `snap`, straight bars end
/// flat, half a thickness short of their centerline's ends: on the pixel
/// grid too, leaving the corner where bars meet dark.
fn bar(from: [f64; 2], to: [f64; 2], thickness: f64, snap: bool) -> Vec<[f64; 2]> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let len = (dx * dx + dy * dy).sqrt().max(f64::EPSILON);
    let (ux, uy) = (dx / len, dy / len);
    let (nx, ny) = (-uy * thickness / 2.0, ux * thickness / 2.0);
    let flat = snap && (dx == 0.0 || dy == 0.0);
    let (gap, tip) = match flat {
        true => (thickness / 2.0, 0.0),
        false => (thickness * 0.6, thickness / 2.0),
    };
    let a = [from[0] + ux * gap, from[1] + uy * gap];
    let b = [to[0] - ux * gap, to[1] - uy * gap];
    vec![
        [a[0] - ux * tip, a[1] - uy * tip],
        [a[0] + nx, a[1] + ny],
        [b[0] + nx, b[1] + ny],
        [b[0] + ux * tip, b[1] + uy * tip],
        [b[0] - nx, b[1] - ny],
        [a[0] - nx, a[1] - ny],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_follow_the_bit_layout() {
        assert_eq!(mask(SegmentStyle::Alpha14, 'x'), 0x5500);
        assert_eq!(mask(SegmentStyle::Alpha14, ' '), 0);
        assert_eq!(mask(SegmentStyle::Numeric7, '8'), 0x7F);
        assert_eq!(mask(SegmentStyle::Numeric7, 'A'), 0);
    }

    #[test]
    fn one_polygon_per_lit_segment() {
        let cell = [0.0, 0.0, 8.0, 16.0];
        // '1' in 14 segments: b, c and the upper right diagonal
        assert_eq!(
            polygons(SegmentStyle::Alpha14, 0x0406, cell, false).len(),
            3
        );
        assert_eq!(polygons(SegmentStyle::Numeric7, 0x7F, cell, false).len(), 7);
        assert_eq!(polygons(SegmentStyle::Numeric7, 0x80, cell, false).len(), 1);
    }

    #[test]
    fn snapped_bars_cover_whole_pixel_rows_and_columns() {
        // A 12x18 cell: bars come out 1.2 thick and the middle one centered
        // on the boundary between rows 8 and 9.
        let cell = [3.0, 2.0, 12.0, 18.0];
        let whole = |v: f64| v.fract() == 0.0;
        for (bit, horizontal) in [(0, true), (6, true), (3, true), (1, false), (4, false)] {
            let bar = &polygons(SegmentStyle::Numeric7, 1 << bit, cell, true)[0];
            // Points 1 and 5 are the two long edges at one end of the bar.
            let axis = usize::from(horizontal);
            let (one, other) = (bar[1][axis], bar[5][axis]);
            assert!(
                whole(one) && whole(other),
                "segment {bit}: {one} and {other}"
            );
            assert_eq!((one - other).abs(), 1.0, "segment {bit}");
            // Flat ends, on the grid as well.
            let along = 1 - axis;
            assert_eq!(bar[0][along], bar[1][along]);
            assert!(
                whole(bar[0][along]) && whole(bar[3][along]),
                "segment {bit}"
            );
        }
        let loose = &polygons(SegmentStyle::Numeric7, 1 << 6, cell, false)[0];
        assert!(!whole(loose[1][1]));
    }

    #[test]
    fn masks_justify_cut_and_fold_dots() {
        let seven = SegmentStyle::Numeric7;
        assert_eq!(masks(seven, "12", 4, Justify::Left), [0x06, 0x5B, 0, 0]);
        assert_eq!(masks(seven, "12", 4, Justify::Right), [0, 0, 0x06, 0x5B]);
        // too long: left keeps the start, right keeps the end
        assert_eq!(masks(seven, "123", 2, Justify::Left), [0x06, 0x5B]);
        assert_eq!(masks(seven, "123", 2, Justify::Right), [0x5B, 0x4F]);
        // the separator lights the previous cell's dot
        assert_eq!(
            masks(seven, "1,2", 3, Justify::Right),
            [0, 0x06 | 0x80, 0x5B]
        );
        assert_eq!(masks(seven, ".", 1, Justify::Left), [0x80]);
    }
}
