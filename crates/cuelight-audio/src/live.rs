use crate::mixer::Mixer;
use crate::sound::Sound;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use cuelight::Voice;
use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What the game thread tells the mixer on the device thread.
enum Command {
    Sound(String, Arc<Sound>),
    Voices(Vec<Voice>),
}

/// How long the device stays open after the last voice stops, so the gaps
/// in a show do not open and close it over and over.
const LINGER: Duration = Duration::from_secs(3);

/// How long to wait before trying a device that would not open again.
const RETRY: Duration = Duration::from_secs(5);

/// The open device and the mixer running on its thread.
struct Live {
    commands: Sender<Command>,
    _stream: cpal::Stream,
}

/// The default sound device playing a [`Mixer`], opened only while there
/// is something to hear.
///
/// The device asks for blocks on its own thread and the mixer runs there;
/// this side only queues commands: sounds to know and, once per frame, the
/// engine's voice list.
///
/// Opening this does not open the device. A show is mostly silence, and an
/// audio stream that exists all the same is one the desktop lists as an
/// application making sound, which is untrue and hard to ignore in a
/// volume mixer. The device opens on the first voice and closes again
/// after a few seconds of quiet. A voice carries the position it should be
/// at, so a sound that starts while the device is opening is heard from
/// where it would be rather than late. Dropping this stops the sound.
pub struct Output {
    device: cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    rate: u32,
    channels: u16,
    /// The sounds the mixer should know. Kept on this side because the
    /// mixer goes away with the device and has to be told again.
    sounds: BTreeMap<String, Arc<Sound>>,
    live: Option<Live>,
    /// Since when nothing has been playing.
    quiet_since: Option<Instant>,
    /// When it is worth trying a device that failed to open again.
    retry_at: Option<Instant>,
}

impl Output {
    /// Find the default output device and the configuration it wants.
    /// Nothing is opened until something plays.
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
        log::info!(
            "audio: {} at {rate} Hz, {channels} channel(s), {format}; \
             opened while something is playing",
            device
                .description()
                .map_or_else(|_| "output device".to_owned(), |d| d.to_string())
        );
        Ok(Output {
            device,
            config,
            format,
            rate,
            channels,
            sounds: BTreeMap::new(),
            live: None,
            quiet_since: None,
            retry_at: None,
        })
    }

    /// The device's sample rate.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Whether the device is open at the moment.
    pub fn is_open(&self) -> bool {
        self.live.is_some()
    }

    /// Hand the mixer the samples of sound `name`.
    pub fn set_sound(&mut self, name: &str, sound: Arc<Sound>) {
        if let Some(live) = &self.live {
            let _ = live
                .commands
                .send(Command::Sound(name.to_owned(), sound.clone()));
        }
        self.sounds.insert(name.to_owned(), sound);
    }

    /// Hand the mixer this frame's voice list; see [`Mixer::apply`]. This
    /// is also what opens and closes the device.
    pub fn apply(&mut self, voices: &[Voice]) {
        if voices.is_empty() {
            let quiet_since = *self.quiet_since.get_or_insert_with(Instant::now);
            if self.live.is_some() && quiet_since.elapsed() >= LINGER {
                log::debug!("audio: nothing playing, closing the device");
                self.live = None;
                return;
            }
        } else {
            self.quiet_since = None;
            if self.live.is_none() {
                self.start();
            }
        }
        if let Some(live) = &self.live {
            let _ = live.commands.send(Command::Voices(voices.to_vec()));
        }
    }

    /// Open the device and tell the fresh mixer every sound it knows.
    fn start(&mut self) {
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        let (tx, rx) = channel();
        let built = match self.format {
            cpal::SampleFormat::F32 => build::<f32>(&self.device, &self.config, rx),
            cpal::SampleFormat::I16 => build::<i16>(&self.device, &self.config, rx),
            cpal::SampleFormat::U16 => build::<u16>(&self.device, &self.config, rx),
            cpal::SampleFormat::I32 => build::<i32>(&self.device, &self.config, rx),
            cpal::SampleFormat::F64 => build::<f64>(&self.device, &self.config, rx),
            other => Err(format!("unsupported sample format {other}")),
        };
        let stream = match built.and_then(|stream| {
            stream
                .play()
                .map_err(|e| format!("starting the stream: {e}"))?;
            Ok(stream)
        }) {
            Ok(stream) => stream,
            Err(e) => {
                log::warn!("audio: {e}");
                self.retry_at = Some(Instant::now() + RETRY);
                return;
            }
        };
        self.retry_at = None;
        // The mixer went away with the last device, so this one is told
        // every sound again before it is told what to play.
        for (name, sound) in &self.sounds {
            let _ = tx.send(Command::Sound(name.clone(), sound.clone()));
        }
        log::debug!(
            "audio: something to play, device open with {} sound(s)",
            self.sounds.len()
        );
        self.live = Some(Live {
            commands: tx,
            _stream: stream,
        });
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
