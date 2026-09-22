//! Sound through the browser's WebAudio: the engine's voice list mapped
//! onto buffer sources and gain nodes, which already run on the browser's
//! own audio thread, so there is nothing to mix here.

use cuelight::Voice;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioBuffer, AudioBufferSourceNode, AudioContext, GainNode};

/// Seconds a gain takes to move: keeps starts, stops and gain changes
/// from clicking.
const RAMP: f64 = 0.005;

/// How far a voice may drift from the position the engine reports before
/// it is restarted there: a seek is much more than a frame's worth.
const RESYNC: f64 = 0.25;

/// One voice being played.
struct Playing {
    source: AudioBufferSourceNode,
    gain: GainNode,
    /// Context time at which the sound's position 0 would have played.
    origin: f64,
    duration: f64,
    looping: bool,
}

/// The page's audio context with the show's sounds decoded into it.
pub struct WebAudio {
    context: AudioContext,
    sounds: HashMap<String, AudioBuffer>,
    playing: HashMap<u64, Playing>,
    /// Whether the page wants sound at all; off keeps the context
    /// suspended whatever gestures come in.
    enabled: bool,
}

impl WebAudio {
    /// Open an audio context. Browsers start it suspended until the page
    /// has been interacted with; see [`WebAudio::resume`].
    pub fn new() -> Result<WebAudio, JsValue> {
        Ok(WebAudio {
            context: AudioContext::new()?,
            sounds: HashMap::new(),
            playing: HashMap::new(),
            enabled: true,
        })
    }

    /// Decode a sound file's bytes and keep it under `name`; returns its
    /// duration in seconds, what the engine registers.
    pub async fn decode(&mut self, name: &str, bytes: &[u8]) -> Result<f64, JsValue> {
        let array = js_sys::Uint8Array::from(bytes);
        let buffer: AudioBuffer = JsFuture::from(self.context.decode_audio_data(&array.buffer())?)
            .await?
            .dyn_into()?;
        let duration = buffer.duration();
        self.sounds.insert(name.to_owned(), buffer);
        Ok(duration)
    }

    /// Ask the context to run; only honored from a user gesture, which is
    /// why the player calls it on the first click or key press. Does
    /// nothing while sound is disabled.
    pub fn resume(&self) {
        if self.enabled {
            let _ = self.context.resume();
        }
    }

    /// Whether the page wants sound.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Let sound through, or silence it. Off stops every voice and
    /// suspends the context, and nothing is scheduled again until it is
    /// on: no gesture brings it back, and no stale sources pile up in the
    /// suspended graph to burst out on resume. Plays go on in engine time
    /// meanwhile; when sound returns, the next frame's [`apply`] starts
    /// them afresh at their current positions.
    ///
    /// [`apply`]: WebAudio::apply
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            let _ = self.context.resume();
        } else {
            self.stop_all();
            let _ = self.context.suspend();
        }
    }

    fn stop_all(&mut self) {
        let now = self.context.current_time();
        for id in self.playing.keys().copied().collect::<Vec<_>>() {
            self.stop(id, now);
        }
    }

    /// Whether sound is actually coming out, or the context still waits
    /// for a gesture.
    pub fn running(&self) -> bool {
        self.context.state() == web_sys::AudioContextState::Running
    }

    /// Make what plays match `voices`, the engine's list for this frame:
    /// a new id starts at its position, a missing id fades out, a gain
    /// change ramps, a position far from the voice's is restarted there.
    pub fn apply(&mut self, voices: &[Voice]) {
        // Silenced: nothing plays and nothing is scheduled; see
        // `set_enabled`.
        if !self.enabled {
            self.stop_all();
            return;
        }
        let now = self.context.current_time();
        for voice in voices {
            let drifted = self.playing.get(&voice.id).is_some_and(|p| {
                let mut drift = voice.position - (now - p.origin);
                if p.looping && p.duration > 0.0 {
                    drift = (drift + p.duration / 2.0).rem_euclid(p.duration) - p.duration / 2.0;
                }
                drift.abs() > RESYNC
            });
            if drifted {
                self.stop(voice.id, now);
            }
            match self.playing.get(&voice.id) {
                Some(playing) => {
                    let _ = playing
                        .gain
                        .gain()
                        .set_target_at_time(voice.gain as f32, now, RAMP);
                }
                None => {
                    if let Some(playing) = self.start(voice, now) {
                        self.playing.insert(voice.id, playing);
                    }
                }
            }
        }
        let gone: Vec<u64> = self
            .playing
            .keys()
            .filter(|id| !voices.iter().any(|v| v.id == **id))
            .copied()
            .collect();
        for id in gone {
            self.stop(id, now);
        }
    }

    fn start(&self, voice: &Voice, now: f64) -> Option<Playing> {
        let buffer = self.sounds.get(&voice.sound)?;
        let duration = buffer.duration();
        if !voice.looping && voice.position >= duration {
            return None;
        }
        let source = self.context.create_buffer_source().ok()?;
        source.set_buffer(Some(buffer));
        source.set_loop(voice.looping);
        let gain = self.context.create_gain().ok()?;
        // Ramp in from silence, like every other gain change.
        let param = gain.gain();
        param.set_value(0.0);
        let _ = param.set_target_at_time(voice.gain as f32, now, RAMP);
        source.connect_with_audio_node(&gain).ok()?;
        gain.connect_with_audio_node(&self.context.destination())
            .ok()?;
        let position = voice.position.max(0.0);
        source
            .start_with_when_and_grain_offset(now, position)
            .ok()?;
        Some(Playing {
            source,
            gain,
            origin: now - position,
            duration,
            looping: voice.looping,
        })
    }

    /// Fade the voice out and let its source end shortly after.
    fn stop(&mut self, id: u64, now: f64) {
        let Some(playing) = self.playing.remove(&id) else {
            return;
        };
        let param = playing.gain.gain();
        let _ = param.set_target_at_time(0.0, now, RAMP);
        let source: &web_sys::AudioScheduledSourceNode = &playing.source;
        let _ = source.stop_with_when(now + RAMP * 5.0);
    }
}
