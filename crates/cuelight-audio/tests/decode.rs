//! Decoding a sound file: a WAV written by hound comes back as samples.

use cuelight_audio::{Mixer, Sound, Voice};
use std::io::Cursor;
use std::sync::Arc;

fn wav(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
        for frame in 0..frames {
            for channel in 0..channels {
                // A ramp on the left, its negative on the right.
                let value = (frame as f32 / frames as f32) * i16::MAX as f32;
                let value = if channel == 0 { value } else { -value };
                writer.write_sample(value as i16).unwrap();
            }
        }
        writer.finalize().unwrap();
    }
    cursor.into_inner()
}

#[test]
fn decodes_wav() {
    let sound = Sound::decode("wav", &wav(22050, 2, 2205)).unwrap();
    assert_eq!(sound.rate, 22050);
    assert_eq!(sound.channels, 2);
    assert_eq!(sound.frames(), 2205);
    assert!((sound.duration() - 0.1).abs() < 1e-9);
    // Halfway: half scale, mirrored.
    let mid = 1102 * 2;
    assert!((sound.samples[mid] - 0.5).abs() < 0.01);
    assert!((sound.samples[mid + 1] + 0.5).abs() < 0.01);
}

#[test]
fn rejects_junk() {
    assert!(Sound::decode("wav", b"not a sound").is_err());
    assert!(Sound::decode("ogg", &[]).is_err());
}

#[test]
fn mixes_a_decoded_sound_offline() {
    let sound = Arc::new(Sound::decode("wav", &wav(48000, 1, 4800)).unwrap());
    let mut mixer = Mixer::new(48000);
    mixer.set_sound("ramp", sound);
    mixer.apply(&[Voice {
        id: 1,
        layer: "ramp".into(),
        sound: "ramp".into(),
        position: 0.05,
        gain: 1.0,
        looping: false,
        bus: None,
    }]);
    let mut out = vec![0.0; 2 * 2400];
    mixer.render(&mut out);
    // Started halfway through the ramp; 10 ms later it is at 0.6, well
    // past the 5 ms gain ramp-in.
    assert!((out[2 * 480] - 0.6).abs() < 0.02, "{}", out[2 * 480]);
    // The sound is used up; the next block notices and drops the voice.
    let mut out = vec![0.0; 2 * 8];
    mixer.render(&mut out);
    assert!(!mixer.is_active());
    assert!(out.iter().all(|s| *s == 0.0));
}
