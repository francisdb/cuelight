//! Offscreen rasterization of the engine's resolved show via vello + wgpu.
//!
//! The renderer owns a headless wgpu device and renders into a texture the
//! host can composite; [`Renderer::render_to_rgba`] additionally reads the
//! pixels back for inspection, tests and PNG dumps.
//!
//! The show's output mode (see [`Engine::output`]) is applied to the
//! finished frame: [`Renderer`] does it after readback, hosts rendering on
//! their own device can run [`OutputPass`] on the GPU.

use crate::engine::{Engine, ResolvedShape};
use crate::lru::ByteLru;
use crate::model::{parse_color, DotShape, OutputMode, Pass, Scaling};
use crate::output::{OutputColor, LUMA_WEIGHTS};
use crate::path::PathElement;
use std::collections::HashMap;
use std::sync::Arc;
use vello::kurbo::{Affine, BezPath, Circle, Join, Rect, Stroke};
use vello::peniko::{Blob, Color, Extend, Fill, ImageAlphaType, ImageBrush, ImageFormat, Mix};
use vello::wgpu;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    #[error("engine error: {0}")]
    Engine(#[from] crate::engine::Error),
    #[error("no suitable GPU adapter found")]
    NoAdapter,
    #[error("wgpu device request failed: {0}")]
    Device(String),
    #[error("vello render failed: {0}")]
    Vello(String),
    #[error("texture readback failed: {0}")]
    Readback(String),
    #[error("png encode failed: {0}")]
    Png(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Interns engine images as vello image resources with stable blob ids, so
/// vello's atlas uploads each image once (per [`Engine::set_image`] call)
/// instead of on every frame.
///
/// Keep one alive per engine you render; [`Renderer`] owns its own. A fresh
/// cache per frame still renders correctly, just without upload reuse.
pub struct ImageCache {
    entries: HashMap<String, (u64, vello::peniko::ImageData)>,
    /// Sprite sheet cells cut out of registered images, by image name and
    /// cell rectangle, with the image revision they were cut from.
    cells: HashMap<(String, [u32; 4]), (u64, vello::peniko::ImageData)>,
    /// Engine-generated bitmaps (text) by revision.
    bitmaps: ByteLru<u64, vello::peniko::ImageData>,
    /// See [`ImageCache::keepalive`].
    keepalive: Option<vello::peniko::ImageData>,
    /// Outline fonts by registration revision: vello caches glyph outlines
    /// per font blob, so the blob has to stay the same across frames.
    fonts: HashMap<u64, vello::peniko::FontData>,
}

/// Budget for cached bitmap uploads; text that changes every frame would
/// otherwise accumulate. Least recently used go first, and dropping one
/// only costs a re-upload.
const MAX_BITMAP_BYTES: usize = 32 * 1024 * 1024;

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            cells: HashMap::new(),
            bitmaps: ByteLru::new(MAX_BITMAP_BYTES),
            keepalive: None,
            fonts: HashMap::new(),
        }
    }
}

fn peniko_image(data: &crate::engine::ImageData) -> vello::peniko::ImageData {
    vello::peniko::ImageData {
        data: Blob::new(Arc::new(data.pixels.clone())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: data.width,
        height: data.height,
    }
}

impl ImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&mut self, name: &str, data: &crate::engine::ImageData) -> vello::peniko::ImageData {
        match self.entries.get(name) {
            Some((revision, image)) if *revision == data.revision() => image.clone(),
            _ => {
                let image = peniko_image(data);
                self.entries
                    .insert(name.to_owned(), (data.revision(), image.clone()));
                image
            }
        }
    }

    /// One transparent pixel that every scene draws. vello 0.10 keeps its
    /// image atlas across frames but throws it away on a frame without any
    /// image, while still believing the images it uploaded earlier are in
    /// it: from then on they render as nothing. Never handing it an
    /// image-less scene avoids that. Remove once vello ships
    /// <https://github.com/linebender/vello/pull/1664>.
    fn keepalive(&mut self) -> vello::peniko::ImageData {
        self.keepalive
            .get_or_insert_with(|| vello::peniko::ImageData {
                data: Blob::new(Arc::new([0u8; 4])),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: 1,
                height: 1,
            })
            .clone()
    }

    fn font(&mut self, font: &crate::engine::FontData) -> vello::peniko::FontData {
        self.fonts
            .entry(font.revision())
            .or_insert_with(|| {
                vello::peniko::FontData::new(Blob::new(Arc::new(font.data.clone())), 0)
            })
            .clone()
    }

    fn bitmap(&mut self, data: &crate::engine::ImageData) -> vello::peniko::ImageData {
        if let Some(image) = self.bitmaps.get(&data.revision()) {
            return image.clone();
        }
        let image = peniko_image(data);
        self.bitmaps
            .insert(data.revision(), image.clone(), data.pixels.len());
        image
    }

    /// One sheet cell as its own image, so sampling at its edges never
    /// reads neighboring cells (bilinear filtering would bleed them in).
    fn cell(
        &mut self,
        name: &str,
        data: &crate::engine::ImageData,
        [x, y, w, h]: [u32; 4],
    ) -> vello::peniko::ImageData {
        let key = (name.to_owned(), [x, y, w, h]);
        if let Some((revision, image)) = self.cells.get(&key) {
            if *revision == data.revision() {
                return image.clone();
            }
        }
        // The engine clamps cells to the image, this only guards races.
        let w = w.min(data.width.saturating_sub(x));
        let h = h.min(data.height.saturating_sub(y));
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let start = ((row * data.width + x) * 4) as usize;
            pixels.extend_from_slice(&data.pixels[start..start + (w * 4) as usize]);
        }
        let image = vello::peniko::ImageData {
            data: Blob::new(Arc::new(pixels)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: w,
            height: h,
        };
        self.cells.insert(key, (data.revision(), image.clone()));
        image
    }
}

/// Translate the engine's resolved layers into a vello [`Scene`](vello::Scene).
///
/// Public so hosts that drive their own wgpu surface (for example a windowed
/// app, see `examples/render_to_window.rs`) can reuse the translation instead of
/// going through the offscreen [`Renderer`]. `images` interns pixel uploads
/// across frames; pass the same cache every frame.
///
/// The scene always contains one invisible pixel-sized image, working around
/// vello losing uploaded images after a frame without any (see
/// `ImageCache::keepalive`). Hosts rendering scenes of their own through
/// the same `vello::Renderer` should likewise never submit one without an
/// image.
pub fn build_vello_scene(
    engine: &Engine,
    images: &mut ImageCache,
) -> Result<vello::Scene, RenderError> {
    let mut show = vello::Scene::new();
    show.draw_image(&ImageBrush::new(images.keepalive()), Affine::IDENTITY);
    for layer in engine.resolved_layers()? {
        let [r, g, b, a] = layer.color;
        let alpha = (f64::from(a) / 255.0 * layer.opacity).clamp(0.0, 1.0);
        let color = Color::from_rgba8(r, g, b, (alpha * 255.0).round() as u8);
        let placement = Affine::new(layer.transform.0);
        match layer.shape {
            ResolvedShape::Rect {
                x,
                y,
                width,
                height,
            } => {
                let rect = Rect::new(x, y, x + width, y + height);
                show.fill(Fill::NonZero, placement, color, None, &rect);
            }
            ResolvedShape::Circle { cx, cy, radius } => {
                let circle = Circle::new((cx, cy), radius);
                show.fill(Fill::NonZero, placement, color, None, &circle);
            }
            ResolvedShape::ClipBegin { shape } => match *shape {
                ResolvedShape::Circle { cx, cy, radius } => {
                    let circle = Circle::new((cx, cy), radius);
                    show.push_clip_layer(Fill::NonZero, placement, &circle);
                }
                ResolvedShape::Path { ref elements, .. } => {
                    show.push_clip_layer(Fill::NonZero, placement, &bez_path(elements));
                }
                // Always push something so the matching ClipEnd balances;
                // anything that is not a rect or circle clips nothing.
                other => {
                    let rect = match other {
                        ResolvedShape::Rect {
                            x,
                            y,
                            width,
                            height,
                        } => Rect::new(x, y, x + width, y + height),
                        _ => Rect::new(f64::MIN, f64::MIN, f64::MAX, f64::MAX),
                    };
                    show.push_clip_layer(Fill::NonZero, placement, &rect);
                }
            },
            ResolvedShape::ClipEnd => show.pop_layer(),
            ResolvedShape::GlyphRun {
                font,
                size,
                glyphs,
                border,
            } => {
                let font = images.font(&font);
                let run = || {
                    glyphs.iter().map(|g| vello::Glyph {
                        id: g.id,
                        x: g.x as f32,
                        y: g.y as f32,
                    })
                };
                // The border first, as a stroke of twice its width, then the
                // fill on top: what stays visible is a border outside the
                // glyph.
                if let Some(([r, g, b, a], width)) = border {
                    let alpha = (f64::from(a) / 255.0 * layer.opacity).clamp(0.0, 1.0);
                    let stroke = Stroke::new(width * 2.0).with_join(Join::Round);
                    show.draw_glyphs(&font)
                        .transform(placement)
                        .font_size(size as f32)
                        .brush(Color::from_rgba8(r, g, b, (alpha * 255.0).round() as u8))
                        .draw(&stroke, run());
                }
                show.draw_glyphs(&font)
                    .transform(placement)
                    .font_size(size as f32)
                    .brush(color)
                    .draw(Fill::NonZero, run());
            }
            ResolvedShape::Path { elements, stroke } => {
                let path = bez_path(&elements);
                if layer.color[3] > 0 {
                    show.fill(Fill::NonZero, placement, color, None, &path);
                }
                if let Some(([r, g, b, a], width)) = stroke {
                    let alpha = (f64::from(a) / 255.0 * layer.opacity).clamp(0.0, 1.0);
                    let stroke = Stroke::new(width);
                    let color = Color::from_rgba8(r, g, b, (alpha * 255.0).round() as u8);
                    show.stroke(&stroke, placement, color, None, &path);
                }
            }
            ResolvedShape::Polygon { points } => {
                let mut path = BezPath::new();
                for (i, &[x, y]) in points.iter().enumerate() {
                    if i == 0 {
                        path.move_to((x, y));
                    } else {
                        path.line_to((x, y));
                    }
                }
                path.close_path();
                show.fill(Fill::NonZero, placement, color, None, &path);
            }
            ResolvedShape::Image {
                image,
                source,
                x,
                y,
                width,
                height,
            } => {
                // Skipped by resolved_layers when unregistered, so the
                // lookup only misses if the host raced a removal.
                let Some(data) = engine.image(&image) else {
                    continue;
                };
                let pixels = match source {
                    None => images.get(&image, data),
                    Some(cell) => images.cell(&image, data, cell),
                };
                let transform = placement
                    * Affine::translate((x, y))
                    * Affine::scale_non_uniform(
                        width / f64::from(pixels.width),
                        height / f64::from(pixels.height),
                    );
                let brush = ImageBrush::new(pixels).with_alpha(layer.opacity as f32);
                show.draw_image(brush.as_ref(), transform);
            }
            ResolvedShape::Bitmap {
                image,
                x,
                y,
                width,
                height,
            } => {
                let brush = ImageBrush::new(images.bitmap(&image)).with_alpha(layer.opacity as f32);
                let transform = placement
                    * Affine::translate((x, y))
                    * Affine::scale_non_uniform(
                        width / f64::from(image.width),
                        height / f64::from(image.height),
                    );
                show.draw_image(brush.as_ref(), transform);
            }
        }
    }
    Ok(show)
}

/// A resolved path as vello draws it.
fn bez_path(elements: &[PathElement]) -> BezPath {
    let mut path = BezPath::new();
    let pt = |[x, y]: [f64; 2]| vello::kurbo::Point::new(x, y);
    for element in elements {
        match *element {
            PathElement::MoveTo(p) => path.move_to(pt(p)),
            PathElement::LineTo(p) => path.line_to(pt(p)),
            PathElement::QuadTo(c, p) => path.quad_to(pt(c), pt(p)),
            PathElement::CubicTo(c1, c2, p) => path.curve_to(pt(c1), pt(c2), pt(p)),
            PathElement::Close => path.close_path(),
        }
    }
    path
}

/// Where a `show` sized canvas lands in a `target` sized surface: uniform
/// scale, centered, as `(scale, x, y)`. With [`Scaling::PixelPerfect`] the
/// scale is a whole number whenever the target is at least the show's
/// size, and the offsets are whole pixels, so nearest-neighbor sampling
/// maps every canvas pixel to an equal block.
pub fn fit(show: [u32; 2], target: [u32; 2], scaling: Scaling) -> (f64, f64, f64) {
    let [show_w, show_h] = show.map(f64::from);
    let [tw, th] = target.map(f64::from);
    let mut scale = (tw / show_w).min(th / show_h);
    let offsets = |scale: f64| ((tw - show_w * scale) / 2.0, (th - show_h * scale) / 2.0);
    if scaling == Scaling::PixelPerfect {
        if scale >= 1.0 {
            scale = scale.floor();
        }
        let (x, y) = offsets(scale);
        return (scale, x.floor(), y.floor());
    }
    let (x, y) = offsets(scale);
    (scale, x, y)
}

/// The loaded show's declared background as a vello color; opaque black
/// when no show is loaded or the color string does not parse.
pub fn background_color(engine: &Engine) -> Color {
    let bg = engine
        .show()
        .and_then(|s| parse_color(&s.background))
        .unwrap_or([0, 0, 0, 255]);
    Color::from_rgba8(bg[0], bg[1], bg[2], bg[3])
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: vello::Renderer,
    images: ImageCache,
}

impl Renderer {
    /// Create a headless renderer on the first available GPU adapter.
    pub fn new() -> Result<Self, RenderError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .map_err(|_| RenderError::NoAdapter)?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|e| RenderError::Device(e.to_string()))?;
        let renderer = vello::Renderer::new(&device, vello::RendererOptions::default())
            .map_err(|e| RenderError::Vello(e.to_string()))?;
        Ok(Self {
            device,
            queue,
            renderer,
            images: ImageCache::new(),
        })
    }

    /// Render the engine's current state into an offscreen texture and read
    /// it back as tightly-packed RGBA8 bytes (`width * height * 4`).
    pub fn render_to_rgba(&mut self, engine: &Engine) -> Result<RgbaFrame, RenderError> {
        let show_meta = engine
            .show()
            .ok_or(crate::engine::Error::NoShow)
            .map_err(RenderError::Engine)?;
        let [width, height] = show_meta.size;
        let base_color = background_color(engine);

        let vello_scene = build_vello_scene(engine, &mut self.images)?;

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cuelight-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                &vello_scene,
                &view,
                &vello::RenderParams {
                    base_color,
                    width,
                    height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|e| RenderError::Vello(e.to_string()))?;

        // Read the texture back, honoring wgpu's 256-byte row alignment.
        let bytes_per_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cuelight-readback"),
            size: u64::from(bytes_per_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cuelight-readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| RenderError::Readback(format!("{e:?}")))?;
        rx.recv()
            .map_err(|e| RenderError::Readback(e.to_string()))?
            .map_err(|e| RenderError::Readback(e.to_string()))?;

        let mapped = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for row in 0..height {
            let start = (row * bytes_per_row) as usize;
            pixels.extend_from_slice(&mapped[start..start + (width * 4) as usize]);
        }
        drop(mapped);
        buffer.unmap();
        engine.output().apply(&mut pixels);

        Ok(RgbaFrame {
            width,
            height,
            pixels,
        })
    }
}

const OUTPUT_SHADER: &str = r#"
struct Params {
    tint: vec4<f32>,
    // Highest gray level; 0 passes colors through unchanged.
    max_level: f32,
    luma: vec3<f32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(src);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let color = textureLoad(src, vec2<i32>(id.xy), 0);
    var out = color;
    if params.max_level > 0.0 {
        let level = round(dot(color.rgb, params.luma) * params.max_level);
        out = vec4<f32>(params.tint.rgb * level / params.max_level, color.a);
    }
    textureStore(dst, vec2<i32>(id.xy), out);
}
"#;

/// GPU version of [`OutputColor::apply`]: converts a rendered frame into
/// the show's output colors, texture to texture, for hosts that render on
/// their own device and never read pixels back.
pub struct OutputPass {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl OutputPass {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cuelight-output"),
            source: wgpu::ShaderSource::Wgsl(OUTPUT_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cuelight-output"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cuelight-output"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("cuelight-output"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline, layout }
    }

    /// Record the conversion of `src` into `dst`. Both are `width` x
    /// `height`; `src` needs `TEXTURE_BINDING` usage, `dst` must be
    /// `Rgba8Unorm` with `STORAGE_BINDING` usage.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        width: u32,
        height: u32,
        output: OutputColor,
    ) {
        let [r, g, b] = output.tint.map(|c| f32::from(c) / 255.0);
        let max_level = f32::from(output.max_level().unwrap_or(0));
        let [lr, lg, lb] = LUMA_WEIGHTS.map(|w| w as f32);
        // Layout matches Params: vec4 tint, f32 level, then vec3 luma at
        // offset 16 + 16 (vec3 aligns to 16 bytes), struct padded to 48.
        let params: [f32; 12] = [r, g, b, 1.0, max_level, 0.0, 0.0, 0.0, lr, lg, lb, 0.0];
        let bytes: Vec<u8> = params.iter().flat_map(|f| f.to_le_bytes()).collect();
        let uniform = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("cuelight-output-params"),
                contents: &bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cuelight-output"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(dst),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("cuelight-output"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }
}

/// What [`Presenter::present`] hands back for the host to draw.
pub struct Presented {
    /// The engine's frame, placed and scaled for the target: embed it in
    /// the host's own scene, or render it as is.
    pub scene: vello::Scene,
    /// Color for the area around the show (its background, after the
    /// show's output conversion): use it as the render's base color.
    pub base_color: Color,
}

/// Brings an engine's frames onto a host surface the way the show asks
/// for: fitted into the target (see [`fit`]), with the show's output mode
/// applied and its scaling honored.
///
/// Full-color shows with smooth scaling are handed back as vector content
/// under the fit transform, so they render at the target's resolution and
/// stay sharp at any size. Everything else (gray output modes,
/// pixel-perfect scaling) is first rendered at the show's own size, run
/// through the output pass on the GPU and then drawn as an image, because
/// there the pixel grid is the point. That native-size frame is also where
/// post-processing passes belong.
///
/// Keep one per engine and surface; it caches image uploads and GPU
/// targets across frames.
#[derive(Default)]
pub struct Presenter {
    images: ImageCache,
    native: Option<NativeTarget>,
    /// The grille of the dots pass, for the dot size and shape it was made.
    grille: Option<(f64, DotShape, vello::peniko::ImageData)>,
}

/// Pixels across one tile of the dots grille: one dot with its surround.
const GRILLE_TILE: u32 = 64;

/// One tile of the grille a dots pass lays over the frame: black with a
/// transparent dot-shaped hole, its edge softened over a sixteenth of the
/// pitch so dots stay round at a few surface pixels each.
fn grille_tile(size: f64, shape: DotShape) -> vello::peniko::ImageData {
    let n = GRILLE_TILE;
    let radius = size * f64::from(n) / 2.0;
    let mut pixels = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (
                f64::from(x) + 0.5 - f64::from(n) / 2.0,
                f64::from(y) + 0.5 - f64::from(n) / 2.0,
            );
            let distance = match shape {
                DotShape::Square => dx.abs().max(dy.abs()),
                _ => dx.hypot(dy),
            };
            // 0 inside the dot, 1 outside, a soft step in between.
            let soft = f64::from(n) / 16.0;
            let cover = ((distance - radius) / soft + 0.5).clamp(0.0, 1.0);
            pixels.extend([0, 0, 0, (cover * 255.0).round() as u8]);
        }
    }
    vello::peniko::ImageData {
        data: Blob::new(Arc::new(pixels)),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: n,
        height: n,
    }
}

/// The show rendered at its own size, and its converted copy that vello
/// draws from.
struct NativeTarget {
    size: [u32; 2],
    render: wgpu::TextureView,
    converted: wgpu::TextureView,
    image: vello::peniko::ImageData,
    pass: OutputPass,
}

impl NativeTarget {
    fn new(device: &wgpu::Device, renderer: &mut vello::Renderer, size: [u32; 2]) -> Self {
        let texture = |label, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage,
                view_formats: &[],
            })
        };
        let render = texture(
            "cuelight-native",
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let converted = texture(
            "cuelight-native-output",
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        Self {
            size,
            render: render.create_view(&wgpu::TextureViewDescriptor::default()),
            converted: converted.create_view(&wgpu::TextureViewDescriptor::default()),
            image: renderer.register_texture(converted),
            pass: OutputPass::new(device),
        }
    }
}

impl Presenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build what shows `engine`'s current frame in a `target` sized
    /// surface. `renderer` must be the one the host renders the returned
    /// scene with: GPU textures get registered with it.
    pub fn present(
        &mut self,
        engine: &Engine,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut vello::Renderer,
        target: [u32; 2],
    ) -> Result<Presented, RenderError> {
        let show = engine.show().ok_or(crate::engine::Error::NoShow)?;
        let size = show.size;
        let (output, scaling) = (engine.output(), engine.scaling());
        let (scale, x, y) = fit(size, target, scaling);
        let placement = Affine::translate((x, y)) * Affine::scale(scale);
        let content = build_vello_scene(engine, &mut self.images)?;
        let background = background_color(engine);

        let dots = engine
            .passes()
            .into_iter()
            .map(|Pass::Dots(dots)| dots)
            .next();
        let mut scene = vello::Scene::new();
        // Dots are made of canvas pixels, so they need the frame at its own
        // size as well.
        if output.mode == OutputMode::Rgb && scaling == Scaling::Smooth && dots.is_none() {
            // Only the canvas shows: what a show parks outside it must not
            // leak into the letterbox, as it cannot on the native texture.
            let [w, h] = size.map(f64::from);
            scene.push_clip_layer(Fill::NonZero, placement, &Rect::new(0.0, 0.0, w, h));
            scene.append(&content, Some(placement));
            scene.pop_layer();
        } else {
            if self.native.as_ref().is_some_and(|n| n.size != size) {
                if let Some(old) = self.native.take() {
                    renderer.unregister_texture(old.image);
                }
            }
            let native = self
                .native
                .get_or_insert_with(|| NativeTarget::new(device, renderer, size));
            renderer
                .render_to_texture(
                    device,
                    queue,
                    &content,
                    &native.render,
                    &vello::RenderParams {
                        base_color: background,
                        width: size[0],
                        height: size[1],
                        antialiasing_method: vello::AaConfig::Area,
                    },
                )
                .map_err(|e| RenderError::Vello(e.to_string()))?;
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cuelight-present"),
            });
            native.pass.encode(
                device,
                &mut encoder,
                &native.render,
                &native.converted,
                size[0],
                size[1],
                output,
            );
            queue.submit([encoder.finish()]);
            renderer.mark_override_image_dirty(&native.image);
            let mut brush = ImageBrush::new(native.image.clone());
            // A dot needs a few surface pixels to be one.
            let dots = dots.filter(|_| scale >= 3.0);
            if scaling == Scaling::PixelPerfect || dots.is_some() {
                // Nearest neighbor: every canvas pixel becomes a crisp block.
                brush = brush.with_quality(vello::peniko::ImageQuality::Low);
            }
            match dots {
                None => scene.draw_image(&brush, placement),
                Some(dots) => {
                    let [w, h] = size.map(f64::from);
                    let frame = Rect::new(0.0, 0.0, w, h);
                    let smooth = ImageBrush::new(native.image.clone());
                    // Dots that are off still show: nothing gets darker
                    // than their color.
                    let unlit = dots.unlit.as_deref().and_then(parse_color);
                    if let Some([r, g, b, _]) = unlit {
                        let color = Color::from_rgba8(r, g, b, 255);
                        scene.fill(Fill::NonZero, placement, color, None, &frame);
                        scene.push_layer(Fill::NonZero, Mix::Lighten, 1.0, placement, &frame);
                    }
                    scene.draw_image(&brush, placement);
                    if unlit.is_some() {
                        scene.pop_layer();
                    }
                    // The grille: one tile per canvas pixel, repeated.
                    let grille = match &self.grille {
                        Some((s, shape, tile)) if *s == dots.size && *shape == dots.shape => {
                            tile.clone()
                        }
                        _ => {
                            let tile = grille_tile(dots.size, dots.shape);
                            self.grille = Some((dots.size, dots.shape, tile.clone()));
                            tile
                        }
                    };
                    let tiles = ImageBrush::new(grille).with_extend(Extend::Repeat);
                    let per_pixel = Affine::scale(1.0 / f64::from(GRILLE_TILE));
                    scene.fill(Fill::NonZero, placement, &tiles, Some(per_pixel), &frame);
                    // Glow: the frame once more, smoothly scaled so every
                    // pixel spreads into its neighbors, added on top.
                    if dots.glow > 0.0 {
                        scene.push_layer(
                            Fill::NonZero,
                            Mix::Screen,
                            dots.glow as f32,
                            placement,
                            &frame,
                        );
                        scene.draw_image(&smooth, placement);
                        scene.pop_layer();
                    }
                }
            }
        }

        let [r, g, b, a] = output.apply_pixel(background.to_rgba8().to_u8_array());
        Ok(Presented {
            scene,
            base_color: Color::from_rgba8(r, g, b, a),
        })
    }
}

/// A rendered frame: tightly packed RGBA8 pixels.
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RgbaFrame {
    /// Write the frame as a PNG file.
    pub fn write_png(&self, path: impl AsRef<std::path::Path>) -> Result<(), RenderError> {
        let file = std::fs::File::create(path)?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| RenderError::Png(e.to_string()))?;
        writer
            .write_image_data(&self.pixels)
            .map_err(|e| RenderError::Png(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{fit, Scaling};

    #[test]
    fn fit_letterboxes_smoothly_by_default() {
        assert_eq!(
            fit([128, 32], [300, 100], Scaling::Smooth),
            (300.0 / 128.0, 0.0, 12.5)
        );
    }

    #[test]
    fn pixel_perfect_fit_uses_whole_pixels() {
        // 300 / 128 = 2.34 -> 2x, centered on whole pixels.
        assert_eq!(
            fit([128, 32], [300, 100], Scaling::PixelPerfect),
            (2.0, 22.0, 18.0)
        );
        // Smaller than the show: shrink (fractionally), never zero.
        assert_eq!(
            fit([128, 32], [64, 64], Scaling::PixelPerfect),
            (0.5, 0.0, 24.0)
        );
    }
}
