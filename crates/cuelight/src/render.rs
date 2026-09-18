//! Offscreen rasterization of the engine's resolved scene via vello + wgpu.
//!
//! The renderer owns a headless wgpu device and renders into a texture the
//! host can composite; [`Renderer::render_to_rgba`] additionally reads the
//! pixels back for inspection, tests and PNG dumps.

use crate::engine::{Engine, ResolvedShape};
use crate::model::parse_color;
use std::collections::HashMap;
use std::sync::Arc;
use vello::kurbo::{Affine, Circle, Rect};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageFormat};
use vello::wgpu;

#[derive(Debug, thiserror::Error)]
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
}

impl ImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&mut self, name: &str, data: &crate::engine::ImageData) -> vello::peniko::ImageData {
        match self.entries.get(name) {
            Some((revision, image)) if *revision == data.revision() => image.clone(),
            _ => {
                let image = vello::peniko::ImageData {
                    data: Blob::new(Arc::new(data.pixels.clone())),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: data.width,
                    height: data.height,
                };
                self.entries
                    .insert(name.to_owned(), (data.revision(), image.clone()));
                image
            }
        }
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
    let mut scene = vello::Scene::new();
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
                scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
            }
            ResolvedShape::Circle { cx, cy, radius } => {
                let circle = Circle::new((cx, cy), radius);
                scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &circle);
            }
            ResolvedShape::Image {
                image,
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
                let brush =
                    ImageBrush::new(images.get(&image, data)).with_alpha(layer.opacity as f32);
                let transform = Affine::translate((x, y))
                    * Affine::scale_non_uniform(
                        width / f64::from(data.width),
                        height / f64::from(data.height),
                    );
                scene.draw_image(brush.as_ref(), transform);
            }
        }
    }
    Ok(scene)
}

/// The loaded scene's declared background as a vello color; opaque black
/// when no scene is loaded or the color string does not parse.
pub fn background_color(engine: &Engine) -> Color {
    let bg = engine
        .scene()
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
        let scene_meta = engine
            .scene()
            .ok_or(crate::engine::Error::NoScene)
            .map_err(RenderError::Engine)?;
        let [width, height] = scene_meta.size;
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

        Ok(RgbaFrame {
            width,
            height,
            pixels,
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
