use crate::mixer::Mixer;
use crate::sound::Sound;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use cuelight::Voice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

/// What the game thread tells the mixer on the device thread.
enum Command {
    Sound(String, Arc<Sound>),
    Voices(Vec<Voice>),
}

/// The default sound device playing a [`Mixer`].
///
/// The device asks for blocks on its own thread and the mixer runs there;
/// this side only queues commands: sounds to know and, once per frame, the
/// engine's voice list. Dropping it stops the sound and closes the device.
///
/// Opening one opens the device and holds it for as long as it lives. Both
/// halves of that are deliberate.
///
/// Whether to open it at all is the host's call, not this type's: a host
/// asks [`Show::has_sound`](cuelight::Show::has_sound) and does not build
/// an `Output` for a show that cannot make a sound, so such a show is
/// never listed by a desktop as an application making one.
///
/// For a show that can, the device is opened at once rather than at its
/// first cue. An idle sound card is free to suspend and waking one is slow
/// enough to hear: an HDMI output here takes about six hundred
/// milliseconds, which is a cue arriving visibly after the picture it
/// belongs to. Opening early spends that while the show is loading, where
/// nobody is listening, instead of inserting it in front of the first
/// sound. Finding the device and starting the stream takes about ten
/// milliseconds, so it does not hold up the show.
pub struct Output {
    commands: Sender<Command>,
    rate: u32,
    channels: u16,
    /// Set by the device's own thread the first time it asks for audio.
    ready: Arc<AtomicBool>,
    _stream: cpal::Stream,
}

impl Output {
    /// Open the default output device in its default configuration.
    pub fn open() -> Result<Output, String> {
        let opening = Instant::now();
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no default output device")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("no default output config: {e}"))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let (rate, channels) = (config.sample_rate, config.channels);
        let (tx, rx) = channel();
        let ready = Arc::new(AtomicBool::new(false));
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, rx, &ready),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, rx, &ready),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, rx, &ready),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, rx, &ready),
            cpal::SampleFormat::F64 => build::<f64>(&device, &config, rx, &ready),
            other => return Err(format!("unsupported sample format {other}")),
        }?;
        stream
            .play()
            .map_err(|e| format!("starting the stream: {e}"))?;
        log::info!(
            "audio: {} at {rate} Hz, {channels} channel(s), {format}, open in {:.0} ms",
            device
                .description()
                .map_or_else(|_| "output device".to_owned(), |d| d.to_string()),
            opening.elapsed().as_secs_f64() * 1000.0
        );
        Ok(Output {
            commands: tx,
            rate,
            channels,
            ready,
            _stream: stream,
        })
    }

    /// Whether the device has started asking for audio.
    ///
    /// Opening a stream is quick; a sound card that was suspended waking
    /// up behind it is not, and until it does nothing that is played will
    /// be heard. A host that wants to know whether a cue will be heard
    /// when it is fired, rather than a moment later, can ask this. It goes
    /// true once and stays true.
    ///
    /// Nothing is lost by playing before then: the mixer keeps the start
    /// of a sound whose device woke late rather than skipping into it.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// The device's sample rate.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Hand the mixer the samples of sound `name`.
    pub fn set_sound(&self, name: &str, sound: Arc<Sound>) {
        let _ = self.commands.send(Command::Sound(name.to_owned(), sound));
    }

    /// Hand the mixer this frame's voice list; see [`Mixer::apply`].
    pub fn apply(&self, voices: &[Voice]) {
        let _ = self.commands.send(Command::Voices(voices.to_vec()));
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    commands: Receiver<Command>,
    ready: &Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels.max(1));
    let ready = ready.clone();
    let opened = Instant::now();
    let mut mixer = Mixer::new(config.sample_rate);
    let mut stereo: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _| {
                if !ready.swap(true, Ordering::Relaxed) {
                    log::debug!(
                        "audio: device asking for blocks {:.0} ms after it opened",
                        opened.elapsed().as_secs_f64() * 1000.0
                    );
                }
                while let Ok(command) = commands.try_recv() {
                    match command {
                        Command::Sound(name, sound) => mixer.set_sound(&name, sound),
                        Command::Voices(voices) => mixer.apply(&voices),
                    }
                }
                let frames = data.len() / channels;
                stereo.resize(frames * 2, 0.0);
                mixer.render(&mut stereo);
                // Stereo onto whatever the device has: both channels of a
                // mono device, the first two of a wider one.
                for (frame, out) in stereo
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .zip(data.chunks_exact_mut(channels))
                {
                    for (i, sample) in out.iter_mut().enumerate() {
                        *sample = T::from_sample(match (channels, i) {
                            (1, _) => (frame[0] + frame[1]) * 0.5,
                            (_, 0) => frame[0],
                            (_, 1) => frame[1],
                            _ => 0.0,
                        });
                    }
                }
            },
            |e| log::error!("audio stream: {e}"),
            None,
        )
        .map_err(|e| format!("opening the stream: {e}"))
}
