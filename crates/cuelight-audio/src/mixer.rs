use crate::sound::Sound;
use cuelight::Voice;
use std::collections::HashMap;
use std::sync::Arc;

/// Seconds a gain takes to move: keeps starts, stops and gain changes
/// from clicking.
const RAMP: f64 = 0.005;

/// How far a voice may drift from the position the engine reports before
/// it is moved there. Live, the engine's position is a frame and a device
/// buffer old; a seek is much more than that.
const RESYNC: f64 = 0.25;

/// One voice being played.
#[derive(Debug)]
struct Playing {
    id: u64,
    sound: Arc<Sound>,
    /// Position in the sound, in its own frames; fractional between two.
    cursor: f64,
    looping: bool,
    /// The gain applied now, on its way to `target`.
    gain: f32,
    target: f32,
    /// Fading out, to be dropped once silent.
    ending: bool,
}

/// Mixes the engine's voice list into interleaved stereo samples at one
/// output rate: sounds are resampled linearly, gains ramp over a few
/// milliseconds, a voice that leaves the list fades out.
#[derive(Debug)]
pub struct Mixer {
    rate: u32,
    sounds: HashMap<String, Arc<Sound>>,
    playing: Vec<Playing>,
}

impl Mixer {
    /// A mixer producing samples at `rate` Hz.
    pub fn new(rate: u32) -> Self {
        Self {
            rate: rate.max(1),
            sounds: HashMap::new(),
            playing: Vec::new(),
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Register (or replace) the samples of sound `name`, the one the
    /// engine knows by duration.
    pub fn set_sound(&mut self, name: &str, sound: Arc<Sound>) {
        self.sounds.insert(name.to_owned(), sound);
    }

    /// Make what plays match `voices`, the engine's list for this frame:
    /// a new id starts at its position, a missing id fades out, a gain
    /// change ramps, a position far from the voice's is jumped to. Voices
    /// of sounds without samples are left for a later call.
    pub fn apply(&mut self, voices: &[Voice]) {
        for voice in voices {
            match self.playing.iter_mut().find(|p| p.id == voice.id) {
                Some(playing) => {
                    playing.target = voice.gain as f32;
                    playing.ending = false;
                    let rate = f64::from(playing.sound.rate);
                    let here = playing.cursor / rate;
                    let mut drift = voice.position - here;
                    if playing.looping {
                        // On a ring, the shorter way round.
                        let duration = playing.sound.duration();
                        drift = (drift + duration / 2.0).rem_euclid(duration) - duration / 2.0;
                    }
                    if drift.abs() > RESYNC {
                        playing.cursor = voice.position * rate;
                    }
                }
                None => {
                    let Some(sound) = self.sounds.get(&voice.sound) else {
                        continue;
                    };
                    let cursor = voice.position * f64::from(sound.rate);
                    if !voice.looping && cursor >= sound.frames() as f64 {
                        continue;
                    }
                    self.playing.push(Playing {
                        id: voice.id,
                        sound: sound.clone(),
                        cursor,
                        looping: voice.looping,
                        gain: 0.0,
                        target: voice.gain as f32,
                        ending: false,
                    });
                }
            }
        }
        for playing in &mut self.playing {
            if !voices.iter().any(|v| v.id == playing.id) {
                playing.ending = true;
                playing.target = 0.0;
            }
        }
    }

    /// Whether anything is sounding or fading out.
    pub fn is_active(&self) -> bool {
        !self.playing.is_empty()
    }

    /// Fill `out` (interleaved stereo, an even number of samples) with the
    /// next `out.len() / 2` frames, overwriting it.
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let step_per_sample = (1.0 / (RAMP * f64::from(self.rate))) as f32;
        let mut done = Vec::new();
        for (index, playing) in self.playing.iter_mut().enumerate() {
            let sound = &playing.sound;
            let frames = sound.frames();
            let channels = usize::from(sound.channels);
            let advance = f64::from(sound.rate) / f64::from(self.rate);
            let last = (frames.max(1) - 1) as f64;
            for frame in out.as_chunks_mut::<2>().0 {
                // Ramp toward the wanted gain.
                let delta = playing.target - playing.gain;
                playing.gain += delta.clamp(-step_per_sample, step_per_sample);
                if playing.ending && playing.gain <= 0.0 {
                    done.push(index);
                    break;
                }
                if playing.cursor >= frames as f64 {
                    if playing.looping && frames > 0 {
                        playing.cursor %= frames as f64;
                    } else {
                        done.push(index);
                        break;
                    }
                }
                // Linear interpolation between the two nearest frames.
                let a = playing.cursor.floor().min(last);
                let t = (playing.cursor - a) as f32;
                let b = if playing.looping {
                    (a + 1.0) % frames as f64
                } else {
                    (a + 1.0).min(last)
                };
                let (a, b) = (a as usize * channels, b as usize * channels);
                let sample = |offset: usize| {
                    let x = sound.samples[a + offset];
                    let y = sound.samples[b + offset];
                    (x + (y - x) * t) * playing.gain
                };
                if channels >= 2 {
                    frame[0] += sample(0);
                    frame[1] += sample(1);
                } else {
                    let s = sample(0);
                    frame[0] += s;
                    frame[1] += s;
                }
                playing.cursor += advance;
            }
        }
        for index in done.into_iter().rev() {
            self.playing.remove(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, frames: usize) -> Arc<Sound> {
        Arc::new(Sound {
            rate,
            channels: 1,
            samples: (0..frames).map(|_| 0.5).collect(),
        })
    }

    fn voice(id: u64, position: f64, gain: f64, looping: bool) -> Voice {
        Voice {
            id,
            layer: "l".into(),
            sound: "tone".into(),
            position,
            gain,
            looping,
            bus: None,
        }
    }

    #[test]
    fn plays_ramps_and_stops() {
        let mut mixer = Mixer::new(1000);
        mixer.set_sound("tone", tone(1000, 100));
        mixer.apply(&[voice(1, 0.0, 1.0, false)]);
        let mut out = vec![0.0; 40];
        mixer.render(&mut out);
        // The ramp reaches full gain after 5 ms = 5 frames.
        assert!(out[0] < 0.5 && out[1] < 0.5);
        assert!((out[20] - 0.5).abs() < 1e-6 && (out[21] - 0.5).abs() < 1e-6);
        mixer.apply(&[]);
        mixer.render(&mut out);
        assert!(out[38].abs() < 1e-6);
        assert!(!mixer.is_active());
    }

    #[test]
    fn ends_with_the_sound() {
        let mut mixer = Mixer::new(1000);
        mixer.set_sound("tone", tone(1000, 10));
        mixer.apply(&[voice(1, 0.0, 1.0, false)]);
        let mut out = vec![0.0; 40];
        mixer.render(&mut out);
        assert!(!mixer.is_active());
        assert_eq!(out[30], 0.0);
    }

    #[test]
    fn loops_and_resamples() {
        let mut mixer = Mixer::new(2000);
        mixer.set_sound("tone", tone(1000, 10));
        mixer.apply(&[voice(1, 0.0, 1.0, true)]);
        let mut out = vec![0.0; 200];
        mixer.render(&mut out);
        assert!(mixer.is_active());
        assert!((out[198] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn resyncs_only_when_far() {
        let mut mixer = Mixer::new(1000);
        mixer.set_sound("tone", tone(1000, 5000));
        mixer.apply(&[voice(1, 1.0, 1.0, false)]);
        assert_eq!(mixer.playing[0].cursor, 1000.0);
        mixer.apply(&[voice(1, 1.1, 1.0, false)]);
        assert_eq!(mixer.playing[0].cursor, 1000.0);
        mixer.apply(&[voice(1, 3.0, 1.0, false)]);
        assert_eq!(mixer.playing[0].cursor, 3000.0);
    }

    #[test]
    fn waits_for_samples() {
        let mut mixer = Mixer::new(1000);
        mixer.apply(&[voice(1, 0.0, 1.0, false)]);
        assert!(!mixer.is_active());
        mixer.set_sound("tone", tone(1000, 100));
        mixer.apply(&[voice(1, 0.02, 1.0, false)]);
        assert_eq!(mixer.playing[0].cursor, 20.0);
    }
}
