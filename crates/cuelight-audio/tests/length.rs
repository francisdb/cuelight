//! A sound's length, read from its header rather than decoded.

#[test]
fn a_header_says_how_long_a_sound_is() {
    // A second of silence, as a wav.
    let rate = 44_100u32;
    let frames = rate as usize;
    let mut bytes = Vec::new();
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut bytes), spec).unwrap();
        for _ in 0..frames {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
    }
    let from_header = cuelight_audio::length("wav", &bytes).expect("a wav header says its length");
    let decoded = cuelight_audio::Sound::decode("wav", &bytes)
        .unwrap()
        .duration();
    assert!(
        (from_header - 1.0).abs() < 1e-6,
        "the header says {from_header}s"
    );
    assert!(
        (from_header - decoded).abs() < 1e-6,
        "header {from_header}s against decoded {decoded}s"
    );
}

#[test]
fn an_unreadable_sound_has_no_length() {
    assert_eq!(cuelight_audio::length("wav", b"not a wav"), None);
}
