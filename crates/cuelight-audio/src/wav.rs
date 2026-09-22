use std::path::Path;

/// Save interleaved stereo `samples` at `rate` Hz as a 16-bit PCM WAV
/// file, the form video tools take alongside a frame sequence.
pub fn write_wav(path: impl AsRef<Path>, rate: u32, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for &sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        writer.write_sample(pcm).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())
}
