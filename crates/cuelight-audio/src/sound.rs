use std::io::Cursor;
use std::path::Path;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// File extensions (lowercase) [`Sound::decode`] has a decoder for.
pub const SOUND_EXTENSIONS: &[&str] = &["wav", "flac", "ogg", "mp3"];

/// A decoded sound: interleaved f32 samples at the file's own rate, mono
/// or stereo (more channels are cut to the first two).
#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    pub rate: u32,
    /// 1 or 2.
    pub channels: u16,
    /// `frames * channels` samples, interleaved.
    pub samples: Vec<f32>,
}

impl Sound {
    /// Decode a sound file's `bytes`; `extension` (`"wav"`, `"ogg"`, ...)
    /// hints at the container, the content decides.
    pub fn decode(extension: &str, bytes: &[u8]) -> Result<Sound, String> {
        let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(extension);
        let mut reader = symphonia::default::get_probe()
            .probe(
                &hint,
                source,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| format!("unreadable sound: {e}"))?;
        let track = reader
            .first_track_known_codec(TrackType::Audio)
            .ok_or("no audio track with a known codec")?;
        let Some(CodecParameters::Audio(params)) = &track.codec_params else {
            return Err("no audio codec parameters".into());
        };
        let track_id = track.id;
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported codec: {e}"))?;
        let mut sound = Sound {
            rate: 0,
            channels: 0,
            samples: Vec::new(),
        };
        let mut chunk: Vec<f32> = Vec::new();
        loop {
            let packet = match reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break
                }
                Err(e) => return Err(format!("reading packets: {e}")),
            };
            if packet.track_id != track_id {
                continue;
            }
            let buffer = match decoder.decode(&packet) {
                Ok(buffer) => buffer,
                // A damaged packet is skipped, as players do.
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => return Err(format!("decoding: {e}")),
            };
            let spec = buffer.spec();
            let channels = spec.channels().count().max(1);
            if sound.rate == 0 {
                sound.rate = spec.rate();
                sound.channels = channels.min(2) as u16;
            }
            buffer.copy_to_vec_interleaved(&mut chunk);
            if channels <= 2 {
                sound.samples.extend_from_slice(&chunk);
            } else {
                for frame in chunk.chunks(channels) {
                    sound.samples.extend_from_slice(&frame[..2]);
                }
            }
        }
        if sound.rate == 0 || sound.samples.is_empty() {
            return Err("no samples".into());
        }
        Ok(sound)
    }

    /// Read and decode a sound file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Sound, String> {
        let path = path.as_ref();
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Sound::decode(&extension, &bytes)
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    /// Length in seconds: what the engine registers.
    pub fn duration(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate.max(1))
    }
}
