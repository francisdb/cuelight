//! A sound's length, read from its header rather than decoded.
//!
//! What matters is that the two agree: the render tool registers the
//! header's length and the player registers the decoded one, so a show
//! chained through a sound's `on_end` has to run the same either way.

/// A quarter second of 8 kHz mono tone, one file per format.
const FIXTURES: &[(&str, &[u8])] = &[
    ("ogg", include_bytes!("sounds/quarter.ogg")),
    ("mp3", include_bytes!("sounds/quarter.mp3")),
    ("flac", include_bytes!("sounds/quarter.flac")),
];

#[test]
fn a_header_says_what_decoding_says() {
    for (extension, bytes) in FIXTURES {
        let header = cuelight_audio::length(extension, bytes)
            .unwrap_or_else(|| panic!("{extension}: the header did not say"));
        let decoded = cuelight_audio::Sound::decode(extension, bytes)
            .unwrap_or_else(|e| panic!("{extension}: {e}"))
            .duration();
        assert!(
            (header - decoded).abs() < 1e-6,
            "{extension}: the header says {header}s, decoding says {decoded}s"
        );
        assert!(
            (header - 0.25).abs() < 1e-6,
            "{extension}: {header}s, wanted a quarter second"
        );
    }
}

#[test]
fn a_wav_header_says_how_long_it_is() {
    let rate = 44_100u32;
    let mut bytes = Vec::new();
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut bytes), spec).unwrap();
        for _ in 0..rate {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
    }
    let header = cuelight_audio::length("wav", &bytes).expect("a wav header says its length");
    let decoded = cuelight_audio::Sound::decode("wav", &bytes)
        .unwrap()
        .duration();
    assert!((header - 1.0).abs() < 1e-6, "the header says {header}s");
    assert!(
        (header - decoded).abs() < 1e-6,
        "{header}s against {decoded}s"
    );
}

#[test]
fn an_unreadable_sound_has_no_length() {
    assert_eq!(cuelight_audio::length("wav", b"not a wav"), None);
}
