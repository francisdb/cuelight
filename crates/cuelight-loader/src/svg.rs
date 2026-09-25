//! SVG documents as vector artwork: what usvg makes of a file, reduced to
//! paths with solid fills and strokes.

use cuelight::{Engine, PathElement, Vector, VectorPath};
use std::sync::{Arc, Mutex};
use usvg::tiny_skia_path::PathSegment;

/// Artwork converted from an SVG document, and what it asked for and did
/// not get.
#[derive(Debug, Clone, PartialEq)]
pub struct Artwork {
    pub vector: Vector,
    /// Font families the document named that the show does not ship:
    /// their text is not drawn. A show problem, and the same on every
    /// machine, which is the point of drawing with the show's fonts.
    pub missing_fonts: Vec<String>,
}

/// The fonts an SVG's text is drawn with: a show's own, and no others.
///
/// Text in artwork becomes paths at load, and which paths depends on the
/// font. Taking whatever the machine happens to have installed means a
/// show that draws its text here drops it there, and a browser has
/// nothing to take at all. A show carries its fonts, so those are the
/// ones to draw with.
pub struct SvgFonts {
    fontdb: Arc<usvg::fontdb::Database>,
    /// The family text gets when it names none: the first font the show
    /// registered, so a show with one font need not say which.
    default_family: Option<String>,
}

impl SvgFonts {
    /// The outline fonts registered with `engine`, which are the show's
    /// own: `assets/fonts` after a load, or whatever a host registered.
    ///
    /// Families are matched by the name inside the font file, as an
    /// SVG's `font-family` names them, not by the name the show
    /// registered the file under.
    pub fn of(engine: &Engine) -> SvgFonts {
        let mut fontdb = usvg::fontdb::Database::new();
        for (_, bytes) in engine.outline_fonts() {
            fontdb.load_font_source(usvg::fontdb::Source::Binary(Arc::new(bytes.to_vec())));
        }
        let default_family = fontdb
            .faces()
            .next()
            .and_then(|face| face.families.first().map(|(name, _)| name.clone()));
        SvgFonts {
            fontdb: Arc::new(fontdb),
            default_family,
        }
    }

    /// Parsing options that draw text with these fonts, and a list that
    /// fills with every family they could not answer for.
    fn options(&self) -> (usvg::Options<'static>, Asked) {
        let asked: Asked = Arc::new(Mutex::new(Vec::new()));
        let noted = asked.clone();
        let chosen = usvg::FontResolver::default_font_selector();
        let mut options = usvg::Options {
            fontdb: self.fontdb.clone(),
            ..usvg::Options::default()
        };
        if let Some(family) = &self.default_family {
            options.font_family = family.clone();
        }
        options.font_resolver.select_font = Box::new(move |font, fontdb| {
            let found = chosen(font, fontdb);
            if found.is_none() {
                // What the document asked for, as it wrote it, so the
                // show can be told which font it does not ship.
                let mut noted = noted.lock().unwrap_or_else(|e| e.into_inner());
                for family in font.families() {
                    let name = match family {
                        usvg::FontFamily::Named(name) => name.clone(),
                        other => format!("{other:?}").to_lowercase(),
                    };
                    if !noted.contains(&name) {
                        noted.push(name);
                    }
                }
            }
            found
        });
        (options, asked)
    }
}

/// Families the document asked for and did not get, filled while it is
/// parsed.
type Asked = Arc<Mutex<Vec<String>>>;

/// Convert an SVG document into vector artwork in its own units (the
/// viewBox): every path, in paint order, with group transforms applied
/// and group opacities folded into the colors. Kept: solid fills and
/// strokes (a gradient paints as its first stop's color, a pattern is
/// dropped), fill rule, stroke width. Text becomes paths through the
/// show's own fonts (see [`SvgFonts`]). Dropped: raster images, clip
/// paths, masks, filters, stroke joins and dashes.
pub fn convert_svg(bytes: &[u8], fonts: &SvgFonts) -> Result<Artwork, String> {
    let (options, asked) = fonts.options();
    let tree = usvg::Tree::from_data(bytes, &options).map_err(|e| e.to_string())?;
    let mut paths = Vec::new();
    group(tree.root(), 1.0, &mut paths);
    let missing_fonts = std::mem::take(&mut *asked.lock().unwrap_or_else(|e| e.into_inner()));
    Ok(Artwork {
        vector: Vector {
            width: f64::from(tree.size().width()),
            height: f64::from(tree.size().height()),
            paths,
        },
        missing_fonts,
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
