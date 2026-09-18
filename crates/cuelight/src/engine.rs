use crate::model::{parse_color, Layer, LayerKind, Property, Scene, Shape};
use crate::value::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no scene loaded")]
    NoScene,
    #[error("invalid scene: {0}")]
    InvalidScene(String),
    #[error("invalid color literal {0:?}")]
    InvalidColor(String),
    #[error("invalid image: {0}")]
    InvalidImage(String),
}

/// A host-provided raster image: tightly packed RGBA8 pixels (straight,
/// non-premultiplied alpha), kept in memory and shared with renderers.
#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major RGBA8.
    pub pixels: Arc<[u8]>,
    revision: u64,
}

impl ImageData {
    /// Engine-unique, increasing with every [`Engine::set_image`] call.
    /// Lets renderers cache GPU resources per upload instead of comparing
    /// pixels.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// A running timeline instance.
#[derive(Debug, Clone)]
struct Playhead {
    /// Path to the owning layer within the scene tree.
    layer_path: Vec<usize>,
    /// Timeline index within that layer.
    timeline: usize,
    time: f64,
}

/// The engine: owns the loaded scene and all runtime state.
///
/// Hosts drive it through the four core calls (`load_scene`, `set_variable`,
/// `trigger`, `advance_frame`) and read the result back either as
/// [`resolved_layers`](Engine::resolved_layers) (a flattened draw list, no
/// GPU involved) or through the `render` feature's rasterizer.
#[derive(Debug, Default)]
pub struct Engine {
    scene: Option<Scene>,
    variables: BTreeMap<String, Value>,
    images: BTreeMap<String, ImageData>,
    image_revision: u64,
    playing: Vec<Playhead>,
    time: f64,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a scene from its JSON description, replacing any current scene
    /// and resetting all runtime state. Autoplay timelines start at 0.
    pub fn load_scene(&mut self, json: &str) -> Result<(), Error> {
        let scene: Scene =
            serde_json::from_str(json).map_err(|e| Error::InvalidScene(e.to_string()))?;
        parse_color(&scene.background)
            .ok_or_else(|| Error::InvalidColor(scene.background.clone()))?;
        self.variables = scene.variables.clone();
        self.playing.clear();
        self.time = 0.0;
        self.scene = Some(scene);
        self.start_matching(|tl| tl.autoplay);
        Ok(())
    }

    /// Push a named value from the host. Unknown names are accepted:
    /// content may bind to them later.
    pub fn set_variable(&mut self, name: &str, value: impl Into<Value>) {
        self.variables.insert(name.to_owned(), value.into());
    }

    pub fn variable(&self, name: &str) -> Option<&Value> {
        self.variables.get(name)
    }

    /// Register (or replace) a named RGBA8 image that image layers can
    /// reference. Images are host assets, not scene content: they survive
    /// `load_scene` and may be provided before or after the scene that
    /// uses them (layers referencing a missing image are skipped).
    pub fn set_image(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        pixels: impl Into<Arc<[u8]>>,
    ) -> Result<(), Error> {
        let pixels = pixels.into();
        let expected = width as usize * height as usize * 4;
        if pixels.len() != expected {
            return Err(Error::InvalidImage(format!(
                "{name:?}: {} bytes for {width}x{height}, expected {expected}",
                pixels.len()
            )));
        }
        self.image_revision += 1;
        self.images.insert(
            name.to_owned(),
            ImageData {
                width,
                height,
                pixels,
                revision: self.image_revision,
            },
        );
        Ok(())
    }

    /// The pixels registered under `name`, if any.
    pub fn image(&self, name: &str) -> Option<&ImageData> {
        self.images.get(name)
    }

    /// Fire a named event: every timeline declaring it as its trigger
    /// (re)starts from 0.
    pub fn trigger(&mut self, name: &str) {
        self.start_matching(|tl| tl.trigger.as_deref() == Some(name));
    }

    /// Advance time by `dt` seconds: running timelines progress, looping
    /// ones wrap, finished ones stop (their properties fall back to
    /// bindings/base values).
    pub fn advance_frame(&mut self, dt: f64) {
        self.time += dt;
        let Some(scene) = &self.scene else { return };
        let mut finished: Vec<usize> = Vec::new();
        for (i, p) in self.playing.iter_mut().enumerate() {
            p.time += dt;
            let layer = layer_at(&scene.layers, &p.layer_path);
            let Some(tl) = layer.and_then(|l| l.timelines.get(p.timeline)) else {
                finished.push(i);
                continue;
            };
            let duration = tl.duration();
            if duration <= 0.0 {
                finished.push(i);
            } else if p.time >= duration {
                if tl.looping {
                    p.time %= duration;
                } else {
                    finished.push(i);
                }
            }
        }
        for i in finished.into_iter().rev() {
            self.playing.remove(i);
        }
    }

    /// Seconds advanced since the scene loaded.
    pub fn time(&self) -> f64 {
        self.time
    }

    pub fn scene(&self) -> Option<&Scene> {
        self.scene.as_ref()
    }

    /// Resolve the scene into a flat draw list: visible shape layers in
    /// paint order with absolute position and effective opacity.
    ///
    /// Property precedence, strongest first: running timeline, binding,
    /// base value from the scene description.
    pub fn resolved_layers(&self) -> Result<Vec<ResolvedLayer>, Error> {
        let scene = self.scene.as_ref().ok_or(Error::NoScene)?;
        let mut out = Vec::new();
        self.walk(&scene.layers, &mut Vec::new(), 0.0, 0.0, 1.0, &mut out)?;
        Ok(out)
    }

    fn start_matching(&mut self, want: impl Fn(&crate::model::Timeline) -> bool) {
        let Some(scene) = &self.scene else { return };
        let mut starts = Vec::new();
        collect_timelines(&scene.layers, &mut Vec::new(), &mut |path, idx, tl| {
            if want(tl) {
                starts.push((path.to_vec(), idx));
            }
        });
        for (layer_path, timeline) in starts {
            self.playing
                .retain(|p| !(p.layer_path == layer_path && p.timeline == timeline));
            self.playing.push(Playhead {
                layer_path,
                timeline,
                time: 0.0,
            });
        }
    }

    fn property(&self, layer: &Layer, path: &[usize], prop: Property) -> f64 {
        // Base value from the scene description.
        let mut v = match prop {
            Property::X => layer.x,
            Property::Y => layer.y,
            Property::Opacity => layer.opacity,
            Property::Scale => layer.scale,
        };
        // Bindings override base.
        for b in &layer.bindings {
            if b.property == prop {
                if let Some(value) = self.variables.get(&b.variable) {
                    v = value.as_number() * b.scale + b.offset;
                }
            }
        }
        // A running timeline owns the property.
        for p in &self.playing {
            if p.layer_path != path {
                continue;
            }
            if let Some(tl) = layer.timelines.get(p.timeline) {
                for track in &tl.tracks {
                    if track.property == prop {
                        if let Some(sampled) = track.sample(p.time) {
                            v = sampled;
                        }
                    }
                }
            }
        }
        v
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        layers: &[Layer],
        path: &mut Vec<usize>,
        ox: f64,
        oy: f64,
        oa: f64,
        out: &mut Vec<ResolvedLayer>,
    ) -> Result<(), Error> {
        for (i, layer) in layers.iter().enumerate() {
            path.push(i);
            if layer.visible {
                let x = ox + self.property(layer, path, Property::X);
                let y = oy + self.property(layer, path, Property::Y);
                let opacity = (oa * self.property(layer, path, Property::Opacity)).clamp(0.0, 1.0);
                let scale = self.property(layer, path, Property::Scale);
                match &layer.kind {
                    LayerKind::Group { children } => {
                        self.walk(children, path, x, y, opacity, out)?;
                    }
                    LayerKind::Shape { shape, fill } => {
                        let color =
                            parse_color(fill).ok_or_else(|| Error::InvalidColor(fill.clone()))?;
                        out.push(ResolvedLayer {
                            name: layer.name.clone(),
                            shape: resolve_shape(*shape, x, y, scale),
                            color,
                            opacity,
                        });
                    }
                    LayerKind::Image { image, size } => {
                        // Missing images are skipped, not an error: the
                        // host may provide them later.
                        if let Some(data) = self.images.get(image) {
                            let [width, height] =
                                size.unwrap_or([f64::from(data.width), f64::from(data.height)]);
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape: ResolvedShape::Image {
                                    image: image.clone(),
                                    x,
                                    y,
                                    width: width * scale,
                                    height: height * scale,
                                },
                                color: [255, 255, 255, 255],
                                opacity,
                            });
                        }
                    }
                }
            }
            path.pop();
        }
        Ok(())
    }
}

fn layer_at<'a>(layers: &'a [Layer], path: &[usize]) -> Option<&'a Layer> {
    let (&first, rest) = path.split_first()?;
    let layer = layers.get(first)?;
    if rest.is_empty() {
        return Some(layer);
    }
    match &layer.kind {
        LayerKind::Group { children } => layer_at(children, rest),
        _ => None,
    }
}

fn collect_timelines(
    layers: &[Layer],
    path: &mut Vec<usize>,
    f: &mut impl FnMut(&[usize], usize, &crate::model::Timeline),
) {
    for (i, layer) in layers.iter().enumerate() {
        path.push(i);
        for (idx, tl) in layer.timelines.iter().enumerate() {
            f(path, idx, tl);
        }
        if let LayerKind::Group { children } = &layer.kind {
            collect_timelines(children, path, f);
        }
        path.pop();
    }
}

/// Place a shape's local geometry: scaled uniformly around the layer's
/// x/y origin, then translated to it.
fn resolve_shape(shape: Shape, x: f64, y: f64, scale: f64) -> ResolvedShape {
    match shape {
        Shape::Rect([rx, ry, w, h]) => ResolvedShape::Rect {
            x: rx * scale + x,
            y: ry * scale + y,
            width: w * scale,
            height: h * scale,
        },
        Shape::Circle([cx, cy, r]) => ResolvedShape::Circle {
            cx: cx * scale + x,
            cy: cy * scale + y,
            radius: r * scale,
        },
    }
}

/// One paintable item of the flattened scene, in canvas coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLayer {
    pub name: String,
    pub shape: ResolvedShape,
    /// Fill color as RGBA bytes; opaque white for images.
    pub color: [u8; 4],
    /// Effective opacity in [0, 1] (tree-multiplied).
    pub opacity: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedShape {
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Circle {
        cx: f64,
        cy: f64,
        radius: f64,
    },
    /// A host image (look the pixels up via [`Engine::image`]) drawn into
    /// the destination rectangle.
    Image {
        image: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
}
