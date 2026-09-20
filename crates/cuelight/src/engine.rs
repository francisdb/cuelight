use crate::font::{BitmapFont, Rgba, StyledFont};
use crate::model::{
    parse_color, Align, Binding, Layer, LayerKind, Output, Property, Scaling, Shape, Sheet, Show,
    Timeline,
};
use crate::output::OutputColor;
use crate::value::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("no show loaded")]
    NoShow,
    #[error("invalid show: {0}")]
    InvalidShow(String),
    #[error("invalid color literal {0:?}")]
    InvalidColor(String),
    #[error("invalid image: {0}")]
    InvalidImage(String),
    #[error("invalid font: {0}")]
    InvalidFont(String),
}

/// A host-provided raster image: tightly packed RGBA8 pixels (straight,
/// non-premultiplied alpha), kept in memory and shared with renderers.
#[derive(Debug, Clone, PartialEq)]
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
    /// pixels. Engine-generated bitmaps ([`ResolvedShape::Bitmap`]) number
    /// their revisions in a separate, process-wide sequence.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Wrap engine-generated pixels with a fresh bitmap revision.
    fn generated(rgba: Rgba) -> Self {
        static REVISION: AtomicU64 = AtomicU64::new(0);
        Self {
            width: rgba.width,
            height: rgba.height,
            pixels: rgba.pixels.into(),
            revision: REVISION.fetch_add(1, Ordering::Relaxed) + 1,
        }
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

/// A host-registered bitmap font: its description and page pixels.
#[derive(Debug)]
struct RegisteredFont {
    font: BitmapFont,
    pages: Vec<Rgba>,
}

/// Rasterized text, placed relative to its layer's box.
#[derive(Debug)]
struct TextRaster {
    image: ImageData,
    offset: [i32; 2],
    /// The box the text was laid out in.
    container: [f64; 2],
}

/// Caches for text rendering, filled lazily while resolving layers.
#[derive(Debug, Default)]
struct TextCache {
    /// Styled fonts by style name.
    styled: HashMap<String, Arc<StyledFont>>,
    /// Rasters by style, text, box and alignment; `None` for text that
    /// draws nothing.
    rasters: HashMap<String, Option<Arc<TextRaster>>>,
}

/// Bound on cached text rasters: changing texts (scores) would otherwise
/// grow the cache forever. Clearing it only costs re-rasterizing.
const MAX_CACHED_RASTERS: usize = 512;

/// A running timeline instance.
#[derive(Debug, Clone)]
struct Playhead {
    root: Root,
    /// Path to the owning layer within its root's layer tree.
    layer_path: Vec<usize>,
    /// Timeline index within that layer.
    timeline: usize,
    /// Seconds since the delay ended: negative while delayed.
    time: f64,
}

/// Something the show did that hosts may want to react to; collect them
/// with [`Engine::drain_events`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// The show fired this trigger itself (a timeline's `on_end`). Triggers
    /// the host fires are not echoed back.
    Trigger(String),
}

/// Events kept while the host does not drain them; the oldest are dropped
/// beyond this so an uninterested host costs nothing.
const MAX_PENDING_EVENTS: usize = 256;

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
    fonts: BTreeMap<String, RegisteredFont>,
    text_cache: Mutex<TextCache>,
    playing: Vec<Playhead>,
    active_scene: Option<usize>,
    events: std::collections::VecDeque<Event>,
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
        for output in std::iter::once(&show.output)
            .chain(show.scenes.iter().filter_map(|s| s.output.as_ref()))
        {
            OutputColor::from_output(output)
                .ok_or_else(|| Error::InvalidColor(output.tint.clone().unwrap_or_default()))?;
        }
        validate(&show)?;
        *self.text_cache.get_mut().unwrap_or_else(|e| e.into_inner()) = TextCache::default();
        self.variables = show.variables.clone();
        self.playing.clear();
        self.events.clear();
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

    /// Register (or replace) a named bitmap font that font styles reference
    /// by `file`: a parsed description plus its page images in page order
    /// (see [`BitmapFont::pages`]), each `(width, height, RGBA8 pixels)`.
    /// Like images, fonts are host assets that survive `load_show`.
    pub fn set_font(
        &mut self,
        name: &str,
        font: BitmapFont,
        pages: Vec<(u32, u32, Vec<u8>)>,
    ) -> Result<(), Error> {
        if pages.len() != font.pages().len() {
            return Err(Error::InvalidFont(format!(
                "{name:?}: {} page images for {} pages",
                pages.len(),
                font.pages().len()
            )));
        }
        let pages = pages
            .into_iter()
            .map(|(width, height, pixels)| {
                if pixels.len() != width as usize * height as usize * 4 {
                    return Err(Error::InvalidFont(format!(
                        "{name:?}: page of {} bytes for {width}x{height}",
                        pixels.len()
                    )));
                }
                Ok(Rgba {
                    width,
                    height,
                    pixels,
                })
            })
            .collect::<Result<_, _>>()?;
        self.fonts
            .insert(name.to_owned(), RegisteredFont { font, pages });
        *self.text_cache.get_mut().unwrap_or_else(|e| e.into_inner()) = TextCache::default();
        Ok(())
    }

    /// Whether a bitmap font is registered under `name`.
    pub fn has_font(&self, name: &str) -> bool {
        self.fonts.contains_key(name)
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

    /// The output in effect: what the active scene sets, over the show's.
    fn effective_output(&self) -> Output {
        let Some(show) = &self.show else {
            return Output::default();
        };
        let scene_output = self
            .active_scene
            .and_then(|i| show.scenes.get(i))
            .and_then(|s| s.output.as_ref());
        match scene_output {
            Some(output) => output.over(&show.output),
            None => show.output.clone(),
        }
    }

    /// Output color handling for the current frame (the active scene's
    /// settings over the show's). Full color when no show is loaded.
    /// Renderers apply it to the finished frame.
    pub fn output(&self) -> OutputColor {
        // Tints were validated at load.
        OutputColor::from_output(&self.effective_output()).unwrap_or_default()
    }

    /// How hosts should scale the current frame up to their surface (the
    /// active scene's setting over the show's); see [`render::fit`].
    ///
    /// [`render::fit`]: crate::render::fit
    pub fn scaling(&self) -> Scaling {
        self.effective_output().scaling.unwrap_or_default()
    }

    fn enter_scene(&mut self, scene: usize) {
        self.playing.retain(|p| p.root == Root::Show);
        self.active_scene = Some(scene);
        self.start_matching(Some(Root::Scene(scene)), |tl| tl.autoplay);
    }

    /// Advance time by `dt` seconds: running timelines progress, looping
    /// ones wrap, finished ones stop (their properties fall back to
    /// bindings/base values) and fire their `on_end` trigger, which is
    /// also reported through [`drain_events`](Engine::drain_events).
    pub fn advance_frame(&mut self, dt: f64) {
        self.time += dt;
        let Some(show) = &self.show else { return };
        let mut finished: Vec<usize> = Vec::new();
        let mut on_end: Vec<String> = Vec::new();
        for (i, p) in self.playing.iter_mut().enumerate() {
            p.time += dt;
            let layer = root_layers(show, p.root).and_then(|l| layer_at(l, &p.layer_path));
            let Some(tl) = layer.and_then(|l| l.timelines.get(p.timeline)) else {
                finished.push(i);
                continue;
            };
            if p.time < 0.0 {
                continue;
            }
            let duration = tl.duration();
            if tl.looping && duration > 0.0 {
                p.time %= duration;
            } else if duration <= 0.0 || p.time >= tl.play_time() {
                finished.push(i);
                on_end.extend(tl.on_end.clone());
            }
        }
        for i in finished.into_iter().rev() {
            self.playing.remove(i);
        }
        for name in on_end {
            self.trigger(&name);
            if self.events.len() == MAX_PENDING_EVENTS {
                self.events.pop_front();
            }
            self.events.push_back(Event::Trigger(name));
        }
    }

    /// Take the events raised since the last call, oldest first. This is
    /// how content talks back to the host: a show can end a sequence with
    /// `on_end` and the host reacts (award points, switch hardware, ...).
    pub fn drain_events(&mut self) -> Vec<Event> {
        self.events.drain(..).collect()
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
            let delay = root_layers(show, root)
                .and_then(|layers| layer_at(layers, &layer_path))
                .and_then(|l| l.timelines.get(timeline))
                .map_or(0.0, |tl| tl.delay.max(0.0));
            self.playing.push(Playhead {
                root,
                layer_path,
                timeline,
                time: -delay,
            });
        }
    }

    /// Resolve a layer property: its base value, overridden by bindings,
    /// overridden by a running timeline (numeric properties only). `None`
    /// when this kind of layer does not have the property.
    fn resolve(&self, root: Root, layer: &Layer, path: &[usize], prop: Property) -> Option<Value> {
        let mut v = layer.base_value(prop)?;
        for b in layer.bindings.iter().filter(|b| b.property == prop) {
            if let Some(bound) = self
                .binding_value(b)
                .and_then(|value| self.convert(b, value))
            {
                v = bound;
            }
        }
        if !prop.is_numeric() {
            return Some(v);
        }
        // A running timeline owns the property.
        for p in &self.playing {
            if p.root != root || p.layer_path != path {
                continue;
            }
            let Some(tl) = layer.timelines.get(p.timeline) else {
                continue;
            };
            // Still waiting out its delay: it does not own anything yet.
            let Some(time) = tl.local_time(p.time) else {
                continue;
            };
            for track in tl.tracks.iter().filter(|t| t.property == prop) {
                if let Some(sampled) = track.sample(time) {
                    v = Value::Number(sampled);
                }
            }
        }
        Some(v)
    }

    fn number(&self, root: Root, layer: &Layer, path: &[usize], prop: Property) -> f64 {
        self.resolve(root, layer, path, prop)
            .map_or(0.0, |v| v.as_number())
    }

    fn text(&self, root: Root, layer: &Layer, path: &[usize], prop: Property) -> String {
        self.resolve(root, layer, path, prop)
            .map_or_else(String::new, |v| v.to_text())
    }

    /// The value a binding feeds its property: the variable's, or what
    /// `map`/`default` turn it into. `None` when it does not apply.
    fn binding_value(&self, b: &Binding) -> Option<Value> {
        let value = self.variables.get(&b.variable)?;
        match &b.map {
            None => Some(value.clone()),
            Some(map) => map.get(&value.to_text()).or(b.default.as_ref()).cloned(),
        }
    }

    /// Turn a bound value into what the binding's property holds. `None`
    /// leaves the property as it was.
    fn convert(&self, b: &Binding, value: Value) -> Option<Value> {
        match b.property {
            Property::Text => Some(Value::Text(match value {
                Value::Number(n) => b.format.format(n * b.scale + b.offset),
                other => other.to_text(),
            })),
            // Only declared font styles apply.
            Property::Font => match value {
                Value::Text(style) if self.show.as_ref()?.fonts.contains_key(&style) => {
                    Some(Value::Text(style))
                }
                _ => None,
            },
            _ => Some(Value::Number(value.as_number() * b.scale + b.offset)),
        }
    }

    /// A layer's content box `[x, y, width, height]` in its local space,
    /// scaled; `None` for groups, unregistered images and text whose font
    /// is not registered.
    fn content_box(
        &self,
        root: Root,
        layer: &Layer,
        path: &[usize],
        scale: f64,
    ) -> Option<[f64; 4]> {
        let [x, y, w, h] = match &layer.kind {
            LayerKind::Group { .. } => return None,
            LayerKind::Shape { shape, .. } => match *shape {
                Shape::Rect(rect) => rect,
                Shape::Circle([cx, cy, r]) => [cx - r, cy - r, 2.0 * r, 2.0 * r],
            },
            LayerKind::Image {
                image, size, sheet, ..
            } => {
                let data = self.images.get(image)?;
                let natural = match sheet {
                    Some(sheet) => sheet.cell.map(f64::from),
                    None => [f64::from(data.width), f64::from(data.height)],
                };
                let [w, h] = size.unwrap_or(natural);
                [0.0, 0.0, w, h]
            }
            // The text's box: its size, or the measured text.
            LayerKind::Text { size, align, .. } => {
                let [w, h] = match size {
                    Some(size) => *size,
                    None => {
                        let text = self.text(root, layer, path, Property::Text);
                        let font = self.text(root, layer, path, Property::Font);
                        self.text_raster(&font, &text, None, *align)?.container
                    }
                };
                [0.0, 0.0, w, h]
            }
        };
        Some([x * scale, y * scale, w * scale, h * scale])
    }

    /// Rasterize (or fetch from cache) `text` in font style `style`.
    /// `None` when the style's font is not registered or nothing draws.
    fn text_raster(
        &self,
        style_name: &str,
        text: &str,
        size: Option<[f64; 2]>,
        align: Align,
    ) -> Option<Arc<TextRaster>> {
        let style = self.show.as_ref()?.fonts.get(style_name)?;
        let registered = self.fonts.get(&style.file)?;
        let mut cache = self.text_cache.lock().unwrap_or_else(|e| e.into_inner());
        let key = format!("{style_name}\u{1}{text}\u{1}{size:?}\u{1}{align:?}");
        if let Some(raster) = cache.rasters.get(&key) {
            return raster.clone();
        }
        let styled = cache
            .styled
            .entry(style_name.to_owned())
            .or_insert_with(|| {
                // Colors were validated at load.
                let rgb = |c: &str| {
                    let [r, g, b, _] = parse_color(c).unwrap_or([255; 4]);
                    [r, g, b]
                };
                let border = style.border.as_ref().map(|b| (rgb(&b.color), b.width));
                Arc::new(StyledFont::new(
                    &registered.font,
                    &registered.pages,
                    rgb(&style.color),
                    border,
                ))
            })
            .clone();
        let raster = styled
            .rasterize(text, size, align)
            .map(|(rgba, offset, container)| {
                Arc::new(TextRaster {
                    image: ImageData::generated(rgba),
                    offset,
                    container,
                })
            });
        if cache.rasters.len() >= MAX_CACHED_RASTERS {
            cache.rasters.clear();
        }
        cache.rasters.insert(key, raster.clone());
        raster
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
                let x = ox + self.number(root, layer, path, Property::X);
                let y = oy + self.number(root, layer, path, Property::Y);
                let opacity =
                    (oa * self.number(root, layer, path, Property::Opacity)).clamp(0.0, 1.0);
                let scale = self.number(root, layer, path, Property::Scale);
                // Shift the layer so its anchor point lands on x/y.
                let content_box = layer
                    .anchor
                    .and_then(|anchor| Some((anchor, self.content_box(root, layer, path, scale)?)));
                let (x, y) = match content_box {
                    Some((anchor, [bx, by, bw, bh])) => {
                        let (ax, ay) = anchor.offset(bw, bh, 0.0, 0.0);
                        (x + ax - bx, y + ay - by)
                    }
                    None => (x, y),
                };
                match &layer.kind {
                    LayerKind::Group { children, clip } => {
                        if let Some(clip) = clip {
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape: ResolvedShape::ClipBegin {
                                    shape: Box::new(resolve_shape(*clip, x, y, scale)),
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
                    LayerKind::Image {
                        image, size, sheet, ..
                    } => {
                        // Missing images are skipped, not an error: the
                        // host may provide them later.
                        if let Some(data) = self.images.get(image) {
                            let source = sheet.map(|sheet| {
                                let frame = self.number(root, layer, path, Property::Frame);
                                sheet_cell(sheet, data.width, data.height, frame)
                            });
                            let natural = match source {
                                Some([_, _, w, h]) => [f64::from(w), f64::from(h)],
                                None => [f64::from(data.width), f64::from(data.height)],
                            };
                            let [width, height] = size.unwrap_or(natural);
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape: ResolvedShape::Image {
                                    image: image.clone(),
                                    source,
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
                    LayerKind::Text { size, align, .. } => {
                        let text = self.text(root, layer, path, Property::Text);
                        let font = self.text(root, layer, path, Property::Font);
                        if let Some(raster) = self.text_raster(&font, &text, *size, *align) {
                            let [ox, oy] = raster.offset;
                            out.push(ResolvedLayer {
                                name: layer.name.clone(),
                                shape: ResolvedShape::Bitmap {
                                    x: x + f64::from(ox) * scale,
                                    y: y + f64::from(oy) * scale,
                                    width: f64::from(raster.image.width) * scale,
                                    height: f64::from(raster.image.height) * scale,
                                    image: raster.image.clone(),
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

/// Checks `load_show` does beyond parsing: colors parse, text layers use
/// declared font styles, only numeric properties are keyframed.
fn validate(show: &Show) -> Result<(), Error> {
    for style in show.fonts.values() {
        for color in std::iter::once(&style.color).chain(style.border.as_ref().map(|b| &b.color)) {
            parse_color(color).ok_or_else(|| Error::InvalidColor(color.clone()))?;
        }
    }
    fn layers(show: &Show, list: &[Layer]) -> Result<(), Error> {
        for layer in list {
            if let LayerKind::Text { font, .. } = &layer.kind {
                if !show.fonts.contains_key(font) {
                    return Err(Error::InvalidShow(format!(
                        "layer {:?} uses undeclared font style {font:?}",
                        layer.name
                    )));
                }
            }
            // Properties must exist on this kind of layer; only numeric
            // ones can be keyframed.
            let tracks = layer.timelines.iter().flat_map(|tl| &tl.tracks);
            let used = tracks
                .map(|t| t.property)
                .chain(layer.bindings.iter().map(|b| b.property));
            for property in used {
                if layer.base_value(property).is_none() {
                    return Err(Error::InvalidShow(format!(
                        "layer {:?} has no {property:?} property",
                        layer.name
                    )));
                }
            }
            for timeline in &layer.timelines {
                if timeline.looping && timeline.repeat.is_some() {
                    return Err(Error::InvalidShow(format!(
                        "timeline {:?} of layer {:?} sets both loop and repeat",
                        timeline.name, layer.name
                    )));
                }
                if let Some(track) = timeline.tracks.iter().find(|t| !t.property.is_numeric()) {
                    return Err(Error::InvalidShow(format!(
                        "timeline {:?} of layer {:?} animates {:?}, which can only be bound",
                        timeline.name, layer.name, track.property
                    )));
                }
            }
            for binding in &layer.bindings {
                if binding.property != Property::Font {
                    continue;
                }
                let mapped = binding.map.iter().flat_map(|m| m.values());
                for value in mapped.chain(&binding.default) {
                    let known =
                        matches!(value, Value::Text(style) if show.fonts.contains_key(style));
                    if !known {
                        return Err(Error::InvalidShow(format!(
                            "font binding of layer {:?} maps to {value:?}, not a declared font style",
                            layer.name
                        )));
                    }
                }
            }
            if matches!(layer.kind, LayerKind::Group { .. }) && layer.anchor.is_some() {
                return Err(Error::InvalidShow(format!(
                    "group {:?} has an anchor; groups have no content box",
                    layer.name
                )));
            }
            layers(show, layer.children())?;
        }
        Ok(())
    }
    show.layer_trees().try_for_each(|tree| layers(show, tree))
}

/// The pixel rectangle `[x, y, width, height]` of sheet cell `frame`
/// (rounded down, clamped to the cells that fit the image).
fn sheet_cell(sheet: Sheet, image_width: u32, image_height: u32, frame: f64) -> [u32; 4] {
    let [cw, ch] = sheet.cell.map(|c| c.max(1));
    let columns = sheet.columns.clamp(1, (image_width / cw).max(1));
    let rows = (image_height / ch).max(1);
    let last = columns * rows - 1;
    let index = if frame.is_finite() && frame > 0.0 {
        (frame.floor() as u32).min(last)
    } else {
        0
    };
    [index % columns * cw, index / columns * ch, cw, ch]
}

fn layer_at<'a>(layers: &'a [Layer], path: &[usize]) -> Option<&'a Layer> {
    let (&first, rest) = path.split_first()?;
    let layer = layers.get(first)?;
    if rest.is_empty() {
        return Some(layer);
    }
    layer_at(layer.children(), rest)
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
        collect_timelines(layer.children(), path, f);
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
#[non_exhaustive]
pub struct ResolvedLayer {
    pub name: String,
    pub shape: ResolvedShape,
    /// Fill color as RGBA bytes; opaque white for images.
    pub color: [u8; 4],
    /// Effective opacity in [0, 1] (tree-multiplied).
    pub opacity: f64,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
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
    /// items only show inside `shape` (a rect or circle). Clips nest.
    ClipBegin {
        shape: Box<ResolvedShape>,
    },
    /// End the innermost clip.
    ClipEnd,
    /// A host image (look the pixels up via [`Engine::image`]) drawn into
    /// the destination rectangle: the whole image, or with `source` only
    /// that pixel rectangle `[x, y, width, height]` of it (a sheet cell).
    Image {
        image: String,
        source: Option<[u32; 4]>,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    /// Pixels the engine generated (rasterized text), drawn into the
    /// destination rectangle. `image.revision()` identifies the content.
    Bitmap {
        image: ImageData,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
}
