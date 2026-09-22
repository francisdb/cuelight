use crate::mixer::Mixer;
use crate::sound::Sound;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use cuelight::Voice;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// What the game thread tells the mixer on the device thread.
enum Command {
    Sound(String, Arc<Sound>),
    Voices(Vec<Voice>),
}

/// The default sound device playing a [`Mixer`]. The device asks for
/// blocks on its own thread and the mixer runs there; this side only
/// queues commands: sounds to know and, once per frame, the engine's voice
/// list. Dropping it stops the sound.
pub struct Output {
    commands: Sender<Command>,
    rate: u32,
    channels: u16,
    _stream: cpal::Stream,
}

impl Output {
    /// Open the default output device in its default configuration.
    pub fn open() -> Result<Output, String> {
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
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, rx),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, rx),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, rx),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, rx),
            cpal::SampleFormat::F64 => build::<f64>(&device, &config, rx),
            other => return Err(format!("unsupported sample format {other}")),
        }?;
        stream
            .play()
            .map_err(|e| format!("starting the stream: {e}"))?;
        log::info!(
            "audio: {} at {rate} Hz, {channels} channel(s), {format}",
            device
                .description()
                .map_or_else(|_| "output device".to_owned(), |d| d.to_string())
        );
        Ok(Output {
            commands: tx,
            rate,
            channels,
            _stream: stream,
        })
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
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels.max(1));
    let mut mixer = Mixer::new(config.sample_rate);
    let mut stereo: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _| {
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
