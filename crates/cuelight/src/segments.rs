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

/// How a display is drawn beyond which segments are lit: how fat the
/// bars are, how far they lean, and how much wider this pass is than the
/// segment itself (a glow is the same shapes, grown).
#[derive(Debug, Clone, Copy)]
pub struct Look {
    /// Bar width as a share of the cell's shorter side, at most
    /// [`THICKEST`].
    pub thickness: f64,
    /// Degrees the cell leans; a shear, so the baseline stays level. At
    /// most [`LEANEST`].
    pub slant: f64,
    /// How far the shapes are pushed out in every direction, in canvas
    /// units: a glow pass is the same segments, grown.
    pub grow: f64,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            thickness: 0.1,
            slant: 0.0,
            grow: 0.0,
        }
    }
}

/// The most of the cell's shorter side one bar may be, before the gaps
/// between segments close and a digit stops being legible. See the note
/// in [`polygons`].
pub const THICKEST: f64 = 0.2;

/// The most degrees a cell may lean.
///
/// At 45 a bar steps one pixel per row, which is the most a staircase of
/// whole-pixel strips can step and still have each strip touch the one
/// above it. Further than that the steps come apart, and it stopped
/// being a leaning digit some way before.
pub const LEANEST: f64 = 45.0;

/// Polygons of the lit segments of one digit cell `[x, y, w, h]`.
///
/// With `snap` the bars are a whole number of pixels thick and their long
/// edges lie on pixel boundaries. On a small canvas that decides how a
/// display looks: a one pixel bar centered on a boundary (the middle bar of
/// a cell with an even height) would otherwise be smeared over two rows at
/// half brightness. Every segment is cut into one strip per pixel row
/// there, so a leaning bar, and a diagonal one, come out a staircase of
/// crisp blocks with no edge left for the renderer to smooth.
pub fn polygons(
    style: SegmentStyle,
    mask: u16,
    [x, y, w, h]: [f64; 4],
    snap: bool,
    look: Look,
) -> Vec<Vec<[f64; 2]>> {
    // Gaps grow with the bars, so a fat display stays legible instead of
    // running together. Both are measured against the cell's shorter
    // side, so a cell wider than it is tall does not close the gap
    // between its rows while its columns still look thin.
    //
    // Capped, because past a point they stop being segments: a row and
    // the middle bar have `1/2 - 1.2 * share` of that side between their
    // centerlines and take `share` of it between them, so they meet at
    // 0.227 and overlap beyond. THICKEST leaves about a sixteenth of the
    // side, the narrowest that still reads as two bars.
    let unit = w.min(h);
    let share = look.thickness.clamp(0.01, THICKEST);
    let pad = unit * (share * 1.2);
    let mut thickness = unit * share;
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
        .map(|&(_, from, to)| bar(from, to, thickness, look.grow, snap))
        .collect();
    if mask & DOT != 0 {
        // The dot sits in the bottom-right padding.
        let (cx, cy, r) = (on_grid(x + w - pad / 2.0), y1, thickness / 2.0 + look.grow);
        out.push(vec![
            [cx - r, cy - r],
            [cx + r, cy - r],
            [cx + r, cy + r],
            [cx - r, cy + r],
        ]);
    }
    // A lean is a shear about the cell's baseline, not a rotation: the
    // bars stay square to the row and the digits stay on the line.
    let lean = look.slant.clamp(-LEANEST, LEANEST).to_radians().tan();
    let baseline = y + h;
    if snap {
        // Every segment is cut into one strip per pixel row, the lean
        // taken row by row with it: the upright and level bars because
        // moving their corners alone would leave the sheared edge
        // between them a diagonal, and the diagonal segments because
        // they are diagonals to start with. Either way no edge is left
        // for the renderer to smooth, so a cell holds the three colours
        // a dot display has and not a spread of half-lit ones.
        out = out
            .iter()
            .map(|points| staircase(points, baseline, lean))
            .filter(|points| !points.is_empty())
            .collect();
    } else if lean != 0.0 {
        for points in &mut out {
            for point in points.iter_mut() {
                point[0] -= (point[1] - baseline) * lean;
            }
        }
    }
    out
}

/// The box a polygon fills, as `[x0, y0, x1, y1]`.
fn bounds(points: &[[f64; 2]]) -> [f64; 4] {
    points.iter().fold(
        [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
        |[x0, y0, x1, y1], &[px, py]| [x0.min(px), y0.min(py), x1.max(px), y1.max(py)],
    )
}

/// A shape on a pixel grid: one strip per pixel row, as one outline.
///
/// A row is part of the shape when the shape covers the middle of that
/// row, and takes the columns whose middles it covers there, which is
/// the rule a renderer that did not smooth anything would follow. The
/// lean shears it row by row at the same time, by whole columns.
///
/// Down the left side and back up the right, so the strips are a single
/// shape with no seam between them, and every edge of it is level or
/// upright.
fn staircase(points: &[[f64; 2]], baseline: f64, lean: f64) -> Vec<[f64; 2]> {
    let [_, y0, _, y1] = bounds(points);
    let rows = ((y1 - y0).ceil() as usize).max(1) + 1;
    let mut left = Vec::with_capacity(rows * 4);
    let mut right = Vec::with_capacity(rows * 2);
    for row in y0.floor() as i64..y1.ceil() as i64 {
        let top = row as f64;
        let Some((from, to)) = across(points, top + 0.5) else {
            // Above or below what the shape covers: a segment's pointed
            // ends can reach into a row without reaching its middle.
            continue;
        };
        let shift = ((top + 0.5 - baseline) * lean).round();
        let from = (from - shift).round();
        // A row the shape reaches at all is a row of dots, so the
        // narrowest strip is one dot wide rather than none.
        let to = (to - shift).round().max(from + 1.0);
        left.push([from, top]);
        left.push([from, top + 1.0]);
        right.push([to, top]);
        right.push([to, top + 1.0]);
    }
    right.reverse();
    left.append(&mut right);
    left
}

/// Where a convex outline starts and ends across the line `y`.
fn across(points: &[[f64; 2]], y: f64) -> Option<(f64, f64)> {
    let mut from = f64::MAX;
    let mut to = f64::MIN;
    for edge in 0..points.len() {
        let ([ax, ay], [bx, by]) = (points[edge], points[(edge + 1) % points.len()]);
        let (lo, hi) = (ay.min(by), ay.max(by));
        if y < lo || y > hi {
            continue;
        }
        // A level edge is crossed at both its ends; anything else at the
        // one point where it meets the line.
        let at = match ay == by {
            true => [ax, bx],
            false => [ax + (bx - ax) * (y - ay) / (by - ay); 2],
        };
        from = from.min(at[0]).min(at[1]);
        to = to.max(at[0]).max(at[1]);
    }
    (from <= to).then_some((from, to))
}

/// A segment of `thickness` along `from`-`to`, ends shortened and pointed
/// so touching segments stay visibly apart. With `snap`, straight bars end
/// flat, half a thickness short of their centerline's ends: on the pixel
/// grid too, leaving the corner where bars meet dark.
///
/// `grow` pushes the outline out by that much in every direction, which
/// is what a halo round the segment is. An offset, not a thickness: a
/// bar that only fattened would pull its ends in as it grew, since the
/// gap it leaves at each end is measured from its thickness, and the
/// halo would swell in the middle of the bar instead of following it.
fn bar(from: [f64; 2], to: [f64; 2], thickness: f64, grow: f64, snap: bool) -> Vec<[f64; 2]> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let len = (dx * dx + dy * dy).sqrt().max(f64::EPSILON);
    let (ux, uy) = (dx / len, dy / len);
    let half = thickness / 2.0 + grow;
    let (nx, ny) = (-uy * half, ux * half);
    let flat = snap && (dx == 0.0 || dy == 0.0);
    let (gap, tip) = match flat {
        true => (thickness / 2.0, 0.0),
        false => (thickness * 0.6, thickness / 2.0),
    };
    // The end of a pointed bar is `tip` past where the gap leaves it, so
    // only the gap moves: growing both would push the point out twice.
    // The point blunts as the bar grows, which is what a halo round it
    // should do anyway.
    let gap = gap - grow;
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
            polygons(SegmentStyle::Alpha14, 0x0406, cell, false, Look::default()).len(),
            3
        );
        assert_eq!(
            polygons(SegmentStyle::Numeric7, 0x7F, cell, false, Look::default()).len(),
            7
        );
        assert_eq!(
            polygons(SegmentStyle::Numeric7, 0x80, cell, false, Look::default()).len(),
            1
        );
    }

    #[test]
    fn snapped_bars_cover_whole_pixel_rows_and_columns() {
        // A 12x18 cell: bars come out 1.2 thick and the middle one centered
        // on the boundary between rows 8 and 9.
        let cell = [3.0, 2.0, 12.0, 18.0];
        let whole = |v: f64| v.fract() == 0.0;
        for (bit, horizontal) in [(0, true), (6, true), (3, true), (1, false), (4, false)] {
            let bar = &polygons(
                SegmentStyle::Numeric7,
                1 << bit,
                cell,
                true,
                Look::default(),
            )[0];
            for &[px, py] in bar {
                assert!(whole(px) && whole(py), "segment {bit}: {px}, {py}");
            }
            // One pixel thick across, whichever way it runs.
            let [x0, y0, x1, y1] = bounds(bar);
            let thick = match horizontal {
                true => y1 - y0,
                false => x1 - x0,
            };
            assert_eq!(thick, 1.0, "segment {bit}");
        }
        let loose = &polygons(SegmentStyle::Numeric7, 1 << 6, cell, false, Look::default())[0];
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

    #[test]
    fn a_slant_leans_the_cell_and_keeps_the_baseline() {
        let cell = [0.0, 0.0, 20.0, 40.0];
        let upright = polygons(SegmentStyle::Numeric7, 1 << 0, cell, false, Look::default());
        let leaning = polygons(
            SegmentStyle::Numeric7,
            1 << 0,
            cell,
            false,
            Look {
                slant: 10.0,
                ..Look::default()
            },
        );
        // The top bar sits well above the baseline, so it moves right by
        // its height times the lean.
        let lean = 10f64.to_radians().tan();
        for (a, b) in upright[0].iter().zip(&leaning[0]) {
            let expected = a[0] - (a[1] - 40.0) * lean;
            assert!((b[0] - expected).abs() < 1e-9, "{a:?} -> {b:?}");
            assert_eq!(a[1], b[1], "a shear moves nothing vertically");
        }
        assert!(
            leaning[0][0][0] > upright[0][0][0] + 5.0,
            "and it is visible"
        );
    }

    #[test]
    fn thickness_widens_the_bars_and_the_gaps_with_them() {
        let cell = [0.0, 0.0, 20.0, 40.0];
        let thin = polygons(SegmentStyle::Numeric7, 1 << 1, cell, false, Look::default());
        let fat = polygons(
            SegmentStyle::Numeric7,
            1 << 1,
            cell,
            false,
            Look {
                thickness: 0.2,
                ..Look::default()
            },
        );
        let width = |bar: &Vec<[f64; 2]>| {
            let xs: Vec<f64> = bar.iter().map(|p| p[0]).collect();
            xs.iter().cloned().fold(f64::MIN, f64::max)
                - xs.iter().cloned().fold(f64::MAX, f64::min)
        };
        assert!(width(&fat[0]) > width(&thin[0]) * 1.5, "the bar is fatter");
        // The gaps follow, so the fat one starts further in from the edge.
        let right = |bar: &Vec<[f64; 2]>| bar.iter().map(|p| p[0]).fold(f64::MIN, f64::max);
        assert!(right(&fat[0]) < right(&thin[0]), "and pulled off the edge");
    }

    #[test]
    fn a_fat_display_keeps_a_gap_between_its_segments() {
        // Whatever is asked for, and whatever shape the cell is, the bars
        // stay inside it and stay apart: a digit drawn out of bars that
        // touch is no longer readable as one.
        for cell @ [x, y, w, h] in [
            [0.0, 0.0, 20.0, 40.0],
            [0.0, 0.0, 40.0, 20.0],
            [5.0, 7.0, 24.0, 24.0],
        ] {
            for thickness in [THICKEST, 0.3, 1.0, 40.0] {
                let look = Look {
                    thickness,
                    ..Look::default()
                };
                let bar = |bit: u16| {
                    polygons(SegmentStyle::Numeric7, 1 << bit, cell, false, look)
                        .pop()
                        .expect("one bar")
                };
                let span = |bar: Vec<[f64; 2]>, axis: usize| {
                    bar.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
                        (lo.min(p[axis]), hi.max(p[axis]))
                    })
                };
                let gap = w.min(h) / 50.0;
                // f and b, the two uprights, left and right of the cell.
                let (_, left) = span(bar(5), 0);
                let (right, _) = span(bar(1), 0);
                assert!(right - left > gap, "{cell:?} at {thickness}: uprights");
                // a and g, the top row and the middle one.
                let (top, above) = span(bar(0), 1);
                let (below, _) = span(bar(6), 1);
                assert!(below - above > gap, "{cell:?} at {thickness}: rows");
                // and none of it leaves the cell.
                let (_, bottom) = span(bar(3), 1);
                assert!(top > y && bottom < y + h, "{cell:?} at {thickness}");
                let (outer_left, _) = span(bar(5), 0);
                let (_, outer_right) = span(bar(1), 0);
                assert!(
                    outer_left > x && outer_right < x + w,
                    "{cell:?} at {thickness}"
                );
            }
        }
    }

    #[test]
    fn a_snapped_slant_is_a_staircase_with_no_diagonal_left() {
        // A cell the size a small display gives a character, where one
        // smeared edge is most of the character.
        let cell = [0.0, 0.0, 8.0, 14.0];
        let look = Look {
            slant: 12.0,
            ..Look::default()
        };
        for bar in polygons(SegmentStyle::Numeric7, 0x7F | DOT, cell, true, look) {
            // Every corner on the grid, and every edge between two of
            // them level or upright: a diagonal edge is what the
            // renderer smears across two columns, whole-pixel corners or
            // not.
            for &[px, py] in &bar {
                assert_eq!((px.fract(), py.fract()), (0.0, 0.0), "{px}, {py}");
            }
            for edge in bar
                .windows(2)
                .chain(std::iter::once(&[*bar.last().unwrap(), bar[0]][..]))
            {
                let ([ax, ay], [bx, by]) = (edge[0], edge[1]);
                assert!(
                    ax == bx || ay == by,
                    "{:?} to {:?} is a diagonal",
                    edge[0],
                    edge[1]
                );
            }
        }
        // The steps lean the way the slant says: the top of a cell sits
        // to the right of its baseline.
        let upright = polygons(SegmentStyle::Numeric7, 1 << 0, cell, true, Look::default());
        let leaning = polygons(SegmentStyle::Numeric7, 1 << 0, cell, true, look);
        assert!(bounds(&leaning[0])[0] > bounds(&upright[0])[0]);
        // Off the grid it shears by the exact amount instead.
        let loose = polygons(SegmentStyle::Numeric7, 1 << 0, cell, false, look);
        assert!(loose[0].iter().any(|[px, _]| px.fract() != 0.0));
    }

    #[test]
    fn a_snapped_diagonal_segment_is_a_staircase_too() {
        // X, which is nothing but diagonals, on a cell the size a small
        // display gives a character.
        let cell = [0.0, 0.0, 8.0, 14.0];
        let bars = polygons(SegmentStyle::Alpha14, 0x5500, cell, true, Look::default());
        assert_eq!(bars.len(), 4, "four strokes");
        for bar in &bars {
            for &[px, py] in bar {
                assert_eq!((px.fract(), py.fract()), (0.0, 0.0), "{px}, {py}");
            }
            for edge in bar
                .windows(2)
                .chain(std::iter::once(&[*bar.last().unwrap(), bar[0]][..]))
            {
                let ([ax, ay], [bx, by]) = (edge[0], edge[1]);
                assert!(
                    ax == bx || ay == by,
                    "{:?} to {:?} is a diagonal",
                    edge[0],
                    edge[1]
                );
            }
            // It still goes where a diagonal goes: a stroke of a single
            // column would be an upright bar, not a diagonal.
            let [x0, y0, x1, y1] = bounds(bar);
            assert!(x1 - x0 >= 2.0 && y1 - y0 >= 2.0, "{:?}", bounds(bar));
        }
        // Off the grid it keeps its true shape.
        let loose = polygons(SegmentStyle::Alpha14, 0x5500, cell, false, Look::default());
        assert!(loose[0]
            .iter()
            .any(|[px, py]| px.fract() != 0.0 || py.fract() != 0.0));
    }

    #[test]
    fn a_lean_past_the_cap_is_still_a_display() {
        // Beyond the cap a strip would step further than the one above
        // it is wide, and the staircase would come apart.
        let cell = [0.0, 0.0, 8.0, 14.0];
        let steps = |slant| {
            let look = Look {
                slant,
                ..Look::default()
            };
            let bar = polygons(SegmentStyle::Numeric7, 1 << 1, cell, true, look)
                .pop()
                .expect("one bar");
            // The left side of the outline, one step per pixel row.
            let mut xs: Vec<f64> = bar[..bar.len() / 2].iter().map(|p| p[0]).collect();
            xs.dedup();
            xs.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f64::max)
        };
        assert!(steps(LEANEST) <= 1.0, "{}", steps(LEANEST));
        assert_eq!(steps(80.0), steps(LEANEST), "clamped to the cap");
    }

    #[test]
    fn growing_a_segment_pushes_it_out_all_round() {
        let cell = [0.0, 0.0, 20.0, 40.0];
        let plain = polygons(SegmentStyle::Numeric7, 1 << 0, cell, false, Look::default());
        let grown = polygons(
            SegmentStyle::Numeric7,
            1 << 0,
            cell,
            false,
            Look {
                grow: 4.0,
                ..Look::default()
            },
        );
        // Out by the grow on every side, and along the bar as well: a
        // halo follows the segment instead of swelling in its middle.
        let [x0, y0, x1, y1] = bounds(&plain[0]);
        let [gx0, gy0, gx1, gy1] = bounds(&grown[0]);
        for (grown, plain, way) in [
            (x0 - gx0, 0.0, "left"),
            (gx1 - x1, 0.0, "right"),
            (y0 - gy0, 0.0, "up"),
            (gy1 - y1, 0.0, "down"),
        ] {
            assert!((grown - plain - 4.0).abs() < 1e-9, "{way}: {grown}");
        }
    }
}
