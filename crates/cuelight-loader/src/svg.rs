//! SVG documents as vector artwork: what usvg makes of a file, reduced to
//! paths with solid fills and strokes.

use cuelight::{PathElement, Vector, VectorPath};
use std::sync::{Arc, OnceLock};
use usvg::tiny_skia_path::PathSegment;

/// Convert an SVG document into vector artwork in its own units (the
/// viewBox): every path, in paint order, with group transforms applied
/// and group opacities folded into the colors. Kept: solid fills and
/// strokes (a gradient paints as its first stop's color, a pattern is
/// dropped), fill rule, stroke width. Text becomes paths through the
/// system's fonts. Dropped: raster images, clip paths, masks, filters,
/// stroke joins and dashes.
pub fn convert_svg(bytes: &[u8]) -> Result<Vector, String> {
    let tree = usvg::Tree::from_data(bytes, options()).map_err(|e| e.to_string())?;
    let mut paths = Vec::new();
    group(tree.root(), 1.0, &mut paths);
    Ok(Vector {
        width: f64::from(tree.size().width()),
        height: f64::from(tree.size().height()),
        paths,
    })
}

/// Parsing options, with the system's fonts loaded once for text.
fn options() -> &'static usvg::Options<'static> {
    static OPTIONS: OnceLock<usvg::Options<'static>> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut fontdb = usvg::fontdb::Database::new();
        fontdb.load_system_fonts();
        usvg::Options {
            fontdb: Arc::new(fontdb),
            ..usvg::Options::default()
        }
    })
}

fn group(group: &usvg::Group, opacity: f32, out: &mut Vec<VectorPath>) {
    let opacity = opacity * group.opacity().get();
    for node in group.children() {
        match node {
            usvg::Node::Group(inner) => self::group(inner, opacity, out),
            usvg::Node::Path(path) => {
                if let Some(converted) = convert_path(path, opacity) {
                    out.push(converted);
                }
            }
            usvg::Node::Text(text) => self::group(text.flattened(), opacity, out),
            usvg::Node::Image(_) => {}
        }
    }
}

fn convert_path(path: &usvg::Path, opacity: f32) -> Option<VectorPath> {
    if !path.is_visible() {
        return None;
    }
    let transform = path.abs_transform();
    let point = |p: usvg::tiny_skia_path::Point| {
        let mut p = p;
        transform.map_point(&mut p);
        [f64::from(p.x), f64::from(p.y)]
    };
    let elements: Vec<PathElement> = path
        .data()
        .segments()
        .map(|segment| match segment {
            PathSegment::MoveTo(p) => PathElement::MoveTo(point(p)),
            PathSegment::LineTo(p) => PathElement::LineTo(point(p)),
            PathSegment::QuadTo(c, p) => PathElement::QuadTo(point(c), point(p)),
            PathSegment::CubicTo(c1, c2, p) => PathElement::CubicTo(point(c1), point(c2), point(p)),
            PathSegment::Close => PathElement::Close,
        })
        .collect();
    if elements.is_empty() {
        return None;
    }
    let fill = path
        .fill()
        .and_then(|fill| color(fill.paint(), fill.opacity().get() * opacity));
    // A stroke's width scales with the transform; uniform enough for the
    // artwork this is for.
    let scale = f64::from((transform.sx * transform.sy - transform.kx * transform.ky).abs()).sqrt();
    let stroke = path.stroke().and_then(|stroke| {
        let color = color(stroke.paint(), stroke.opacity().get() * opacity)?;
        Some((color, f64::from(stroke.width().get()) * scale))
    });
    if fill.is_none() && stroke.is_none() {
        return None;
    }
    Some(VectorPath {
        elements,
        fill,
        stroke,
    })
}

/// A paint as one RGBA color: solid colors as they are, gradients by their
/// first stop, patterns not at all.
fn color(paint: &usvg::Paint, opacity: f32) -> Option<[u8; 4]> {
    let (color, stop_opacity) = match paint {
        usvg::Paint::Color(color) => (*color, 1.0),
        usvg::Paint::LinearGradient(gradient) => {
            let stop = gradient.stops().first()?;
            (stop.color(), stop.opacity().get())
        }
        usvg::Paint::RadialGradient(gradient) => {
            let stop = gradient.stops().first()?;
            (stop.color(), stop.opacity().get())
        }
        usvg::Paint::Pattern(_) => return None,
    };
    let alpha = (opacity * stop_opacity).clamp(0.0, 1.0);
    Some([
        color.red,
        color.green,
        color.blue,
        (alpha * 255.0).round() as u8,
    ])
}
