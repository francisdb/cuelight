use crate::model::{parse_color, Layer, LayerKind, Property, Shape, Show, Timeline};
use crate::value::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no show loaded")]
    NoShow,
    #[error("invalid show: {0}")]
    InvalidShow(String),
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

/// Which layer tree a layer path is rooted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    /// The show's own, always present layers.
    Show,
    /// The layers of the scene at this index.
    Scene(usize),
}

/// A running timeline instance.
#[derive(Debug, Clone)]
struct Playhead {
    root: Root,
    /// Path to the owning layer within its root's layer tree.
    layer_path: Vec<usize>,
    /// Timeline index within that layer.
    timeline: usize,
    time: f64,
}

/// The engine: owns the loaded show and all runtime state.
///
/// Hosts drive it through the four core calls (`load_show`, `set_variable`,
/// `trigger`, `advance_frame`) and read the result back either as
/// [`resolved_layers`](Engine::resolved_layers) (a flattened draw list, no
/// GPU involved) or through the `render` feature's rasterizer.
#[derive(Debug, Default)]
pub struct Engine {
    show: Option<Show>,
    variables: BTreeMap<String, Value>,
    images: BTreeMap<String, ImageData>,
    image_revision: u64,
    playing: Vec<Playhead>,
    active_scene: Option<usize>,
    time: f64,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a show from its JSON description, replacing any current show
    /// and resetting all runtime state. The first scene (if any) becomes
    /// active; autoplay timelines start at 0.
    pub fn load_show(&mut self, json: &str) -> Result<(), Error> {
        let show: Show =
            serde_json::from_str(json).map_err(|e| Error::InvalidShow(e.to_string()))?;
        parse_color(&show.background)
            .ok_or_else(|| Error::InvalidColor(show.background.clone()))?;
        self.variables = show.variables.clone();
        self.playing.clear();
        self.time = 0.0;
        self.active_scene = (!show.scenes.is_empty()).then_some(0);
        self.show = Some(show);
        self.start_matching(Some(Root::Show), |tl| tl.autoplay);
        if let Some(scene) = self.active_scene {
            self.start_matching(Some(Root::Scene(scene)), |tl| tl.autoplay);
        }
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
    /// reference. Images are host assets, not show content: they survive
    /// `load_show` and may be provided before or after the show that
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

    /// Fire a named event. A scene declaring it as its trigger becomes the
    /// active scene (restarting it when already active); then every
    /// timeline declaring it, in the show's layers or the active scene,
    /// (re)starts from 0.
    pub fn trigger(&mut self, name: &str) {
        let entered = self.show.as_ref().and_then(|show| {
            show.scenes
                .iter()
                .position(|s| s.trigger.as_deref() == Some(name))
        });
        if let Some(scene) = entered {
            self.enter_scene(scene);
        }
        self.start_matching(None, |tl| tl.trigger.as_deref() == Some(name));
    }

    /// Name of the active scene, if the show has scenes.
    pub fn active_scene(&self) -> Option<&str> {
        let show = self.show.as_ref()?;
        Some(show.scenes.get(self.active_scene?)?.name.as_str())
    }

    fn enter_scene(&mut self, scene: usize) {
        self.playing.retain(|p| p.root == Root::Show);
        self.active_scene = Some(scene);
        self.start_matching(Some(Root::Scene(scene)), |tl| tl.autoplay);
    }

    /// Advance time by `dt` seconds: running timelines progress, looping
    /// ones wrap, finished ones stop (their properties fall back to
    /// bindings/base values).
    pub fn advance_frame(&mut self, dt: f64) {
        self.time += dt;
        let Some(show) = &self.show else { return };
        let mut finished: Vec<usize> = Vec::new();
        for (i, p) in self.playing.iter_mut().enumerate() {
            p.time += dt;
            let layer = root_layers(show, p.root).and_then(|l| layer_at(l, &p.layer_path));
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

    /// Seconds advanced since the show loaded.
    pub fn time(&self) -> f64 {
        self.time
    }

    pub fn show(&self) -> Option<&Show> {
        self.show.as_ref()
    }

    /// Resolve the show into a flat draw list: visible shape layers in
    /// paint order with absolute position and effective opacity. Clipped
    /// groups bracket their children with [`ResolvedShape::ClipBegin`] and
    /// [`ResolvedShape::ClipEnd`].
    ///
    /// Property precedence, strongest first: running timeline, binding,
    /// base value from the show description.
    pub fn resolved_layers(&self) -> Result<Vec<ResolvedLayer>, Error> {
        let show = self.show.as_ref().ok_or(Error::NoShow)?;
        let mut out = Vec::new();
        self.walk(
            Root::Show,
            &show.layers,
            &mut Vec::new(),
            0.0,
            0.0,
            1.0,
            &mut out,
        )?;
        if let Some(scene) = self.active_scene {
            if let Some(layers) = root_layers(show, Root::Scene(scene)) {
                self.walk(
                    Root::Scene(scene),
                    layers,
                    &mut Vec::new(),
                    0.0,
                    0.0,
                    1.0,
                    &mut out,
                )?;
            }
        }
        Ok(out)
    }

    /// (Re)start the timelines `want` selects, in `root` or, with `None`,
    /// in the show's layers and the active scene.
    fn start_matching(&mut self, root: Option<Root>, want: impl Fn(&Timeline) -> bool) {
        let Some(show) = &self.show else { return };
        let roots = match root {
            Some(root) => vec![root],
            None => std::iter::once(Root::Show)
                .chain(self.active_scene.map(Root::Scene))
                .collect(),
        };
        let mut starts = Vec::new();
        for root in roots {
            let Some(layers) = root_layers(show, root) else {
                continue;
            };
            collect_timelines(layers, &mut Vec::new(), &mut |path, idx, tl| {
                if want(tl) {
                    starts.push((root, path.to_vec(), idx));
                }
            });
        }
        for (root, layer_path, timeline) in starts {
            self.playing.retain(|p| {
                !(p.root == root && p.layer_path == layer_path && p.timeline == timeline)
            });
            self.playing.push(Playhead {
                root,
                layer_path,
                timeline,
                time: 0.0,
            });
        }
    }

    fn property(&self, root: Root, layer: &Layer, path: &[usize], prop: Property) -> f64 {
        // Base value from the show description.
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
            if p.root != root || p.layer_path != path {
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
        root: Root,
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
                let x = ox + self.property(root, layer, path, Property::X);
                let y = oy + self.property(root, layer, path, Property::Y);
                let opacity =
                    (oa * self.property(root, layer, path, Property::Opacity)).clamp(0.0, 1.0);
                let scale = self.property(root, layer, path, Property::Scale);
                match &layer.kind {
                    LayerKind::Group { children, clip } => {
                        if let Some([width, height]) = clip {
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape: ResolvedShape::ClipBegin {
                                    x,
                                    y,
                                    width: width * scale,
                                    height: height * scale,
                                },
                                color: [0; 4],
                                opacity,
                            });
                        }
                        self.walk(root, children, path, x, y, opacity, out)?;
                        if clip.is_some() {
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape: ResolvedShape::ClipEnd,
                                color: [0; 4],
                                opacity,
                            });
                        }
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

fn root_layers(show: &Show, root: Root) -> Option<&[Layer]> {
    match root {
        Root::Show => Some(&show.layers),
        Root::Scene(i) => show.scenes.get(i).map(|s| s.layers.as_slice()),
    }
}

fn layer_at<'a>(layers: &'a [Layer], path: &[usize]) -> Option<&'a Layer> {
    let (&first, rest) = path.split_first()?;
    let layer = layers.get(first)?;
    if rest.is_empty() {
        return Some(layer);
    }
    match &layer.kind {
        LayerKind::Group { children, .. } => layer_at(children, rest),
        _ => None,
    }
}

fn collect_timelines(
    layers: &[Layer],
    path: &mut Vec<usize>,
    f: &mut impl FnMut(&[usize], usize, &Timeline),
) {
    for (i, layer) in layers.iter().enumerate() {
        path.push(i);
        for (idx, tl) in layer.timelines.iter().enumerate() {
            f(path, idx, tl);
        }
        if let LayerKind::Group { children, .. } = &layer.kind {
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

/// One paintable item of the flattened show, in canvas coordinates.
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
    /// Start clipping: until the matching [`ResolvedShape::ClipEnd`],
    /// items only show inside this rectangle. Clips nest.
    ClipBegin {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    /// End the innermost clip.
    ClipEnd,
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
