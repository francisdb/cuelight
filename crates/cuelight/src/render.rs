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
use crate::model::{parse_color, Scaling};
use crate::output::{OutputColor, LUMA_WEIGHTS};
use std::collections::HashMap;
use std::sync::Arc;
use vello::kurbo::{Affine, BezPath, Circle, Rect};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageFormat};
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
#[derive(Default)]
pub struct ImageCache {
    entries: HashMap<String, (u64, vello::peniko::ImageData)>,
    /// Sprite sheet cells cut out of registered images, by image name and
    /// cell rectangle, with the image revision they were cut from.
    cells: HashMap<(String, [u32; 4]), (u64, vello::peniko::ImageData)>,
    /// Engine-generated bitmaps (text) by revision.
    bitmaps: HashMap<u64, vello::peniko::ImageData>,
}

/// Bound on cached bitmap uploads; text that changes every frame would
/// otherwise accumulate. Clearing only costs re-uploads.
const MAX_CACHED_BITMAPS: usize = 512;

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

    fn bitmap(&mut self, data: &crate::engine::ImageData) -> vello::peniko::ImageData {
        if let Some(image) = self.bitmaps.get(&data.revision()) {
            return image.clone();
        }
        if self.bitmaps.len() >= MAX_CACHED_BITMAPS {
            self.bitmaps.clear();
        }
        let image = peniko_image(data);
        self.bitmaps.insert(data.revision(), image.clone());
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
pub fn build_vello_scene(
    engine: &Engine,
    images: &mut ImageCache,
) -> Result<vello::Scene, RenderError> {
    let mut show = vello::Scene::new();
    for layer in engine.resolved_layers()? {
        let [r, g, b, a] = layer.color;
        let alpha = (f64::from(a) / 255.0 * layer.opacity).clamp(0.0, 1.0);
        let color = Color::from_rgba8(r, g, b, (alpha * 255.0).round() as u8);
        match layer.shape {
            ResolvedShape::Rect {
                x,
                y,
                width,
                height,
            } => {
                let rect = Rect::new(x, y, x + width, y + height);
                show.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
            }
            ResolvedShape::Circle { cx, cy, radius } => {
                let circle = Circle::new((cx, cy), radius);
                show.fill(Fill::NonZero, Affine::IDENTITY, color, None, &circle);
            }
            ResolvedShape::ClipBegin { shape } => match *shape {
                ResolvedShape::Circle { cx, cy, radius } => {
                    let circle = Circle::new((cx, cy), radius);
                    show.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &circle);
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
                    show.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &rect);
                }
            },
            ResolvedShape::ClipEnd => show.pop_layer(),
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
                show.fill(Fill::NonZero, Affine::IDENTITY, color, None, &path);
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
                let transform = Affine::translate((x, y))
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
                let transform = Affine::translate((x, y))
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
