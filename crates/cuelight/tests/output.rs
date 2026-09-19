//! Output modes: the engine picks the active one, the GPU pass matches the
//! CPU conversion. GPU tests skip (pass) when no adapter is available.

use cuelight::{Engine, OutputColor, OutputMode};

const SHOW: &str = r##"{
  "name": "output",
  "size": [16, 4],
  "output": { "mode": "gray4", "tint": "#FF5820" },
  "scenes": [
    { "name": "dmd", "trigger": "dmd" },
    { "name": "color", "trigger": "color", "output": { "mode": "rgb" } }
  ]
}"##;

#[test]
fn scene_output_overrides_show_output() {
    let mut engine = Engine::new();
    assert_eq!(engine.output(), OutputColor::RGB);
    engine.load_show(SHOW).unwrap();
    let gray = OutputColor {
        mode: OutputMode::Gray4,
        tint: [255, 88, 32],
    };
    assert_eq!(engine.output(), gray);
    engine.trigger("color");
    assert_eq!(engine.output(), OutputColor::RGB);
    engine.trigger("dmd");
    assert_eq!(engine.output(), gray);
}

#[test]
fn invalid_tint_is_rejected() {
    let mut engine = Engine::new();
    let show = SHOW.replace("#FF5820", "orange");
    assert!(engine.load_show(&show).is_err());
}

#[cfg(feature = "render")]
#[test]
fn gpu_output_pass_matches_cpu_conversion() {
    use cuelight::render::OutputPass;
    use cuelight::vello::wgpu;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
        eprintln!("no GPU adapter, skipping");
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();

    // Every gray step plus some saturated colors, as a 16x4 frame.
    let (width, height) = (16u32, 4u32);
    let mut src_pixels = Vec::new();
    for i in 0..width * height {
        let v = (i * 4) as u8;
        let px = match i % 4 {
            0 => [v, v, v, 255],
            1 => [255, v, 0, 255],
            2 => [0, v, 255, 128],
            _ => [v, 0, 255 - v, 255],
        };
        src_pixels.extend_from_slice(&px);
    }

    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = |usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        })
    };
    let src = texture(wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
    let dst = texture(wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC);
    queue.write_texture(
        src.as_image_copy(),
        &src_pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: None,
        },
        extent,
    );

    let output = OutputColor {
        mode: OutputMode::Gray4,
        tint: [255, 88, 32],
    };
    let bytes_per_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(bytes_per_row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    OutputPass::new(&device).encode(
        &device,
        &mut encoder,
        &src.create_view(&Default::default()),
        &dst.create_view(&Default::default()),
        width,
        height,
        output,
    );
    encoder.copy_texture_to_buffer(
        dst.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: None,
            },
        },
        extent,
    );
    queue.submit([encoder.finish()]);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let mapped = buffer.slice(..).get_mapped_range();
    let mut gpu = Vec::new();
    for row in 0..height {
        let start = (row * bytes_per_row) as usize;
        gpu.extend_from_slice(&mapped[start..start + (width * 4) as usize]);
    }

    let mut cpu = src_pixels.clone();
    output.apply(&mut cpu);
    // Float rounding may differ by one step on exact level boundaries.
    for (i, (g, c)) in gpu.iter().zip(&cpu).enumerate() {
        assert!(
            g.abs_diff(*c) <= 1,
            "byte {i}: gpu {g} vs cpu {c} (pixel {:?})",
            &src_pixels[i / 4 * 4..i / 4 * 4 + 4]
        );
    }
}
