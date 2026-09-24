//! Driver scripts: triggers and variable changes with delays, standing in
//! for a live host so a show can be demonstrated and tested repeatably.
//!
//! ```json
//! {
//!   "loop": true,
//!   "steps": [
//!     { "set": { "score": 0 } },
//!     { "wait": 0.5 },
//!     { "trigger": "go" }
//!   ]
//! }
//! ```

use crate::LoadError;
use cuelight::{Engine, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// A scripted command sequence.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Driver {
    /// Start over after the last step.
    #[serde(default, rename = "loop")]
    pub looping: bool,
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// One driver step: exactly one of `wait`, `trigger` or `set`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Step {
    /// Let this many seconds pass.
    Wait { wait: f64 },
    /// Fire a trigger.
    Trigger { trigger: String },
    /// Set variables.
    Set { set: BTreeMap<String, Value> },
}

impl Driver {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let path = path.as_ref();
        let json = std::fs::read_to_string(path).map_err(|source| LoadError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_json(&json).map_err(|message| LoadError::Driver {
            path: path.to_owned(),
            message,
        })
    }

    /// Seconds one pass through the steps takes.
    pub fn duration(&self) -> f64 {
        self.steps
            .iter()
            .map(|s| match s {
                Step::Wait { wait } => wait.max(0.0),
                _ => 0.0,
            })
            .sum()
    }
}

/// Plays a [`Driver`] against an engine as time advances.
#[derive(Debug, Clone)]
pub struct DriverPlayer {
    driver: Driver,
    index: usize,
    wait_left: f64,
    done: bool,
}

impl DriverPlayer {
    pub fn new(driver: Driver) -> Self {
        Self {
            driver,
            index: 0,
            wait_left: 0.0,
            done: false,
        }
    }

    /// Whether the script ran to its end (never, while it loops).
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Advance the script by `dt` seconds: waits consume time, the other
    /// steps apply to `engine` the moment their turn comes. Call it before
    /// `Engine::advance_frame` with the same `dt`. Returns the steps that
    /// were applied, for hosts that log them.
    pub fn advance(&mut self, engine: &mut Engine, mut dt: f64) -> Vec<Step> {
        let mut applied = Vec::new();
        // A looping script without any wait would never yield.
        let mut wrapped = false;
        while !self.done {
            if self.wait_left > 0.0 {
                if dt < self.wait_left {
                    self.wait_left -= dt;
                    break;
                }
                dt -= self.wait_left;
                self.wait_left = 0.0;
            }
            if self.index >= self.driver.steps.len() {
                if !self.driver.looping || self.driver.steps.is_empty() || wrapped {
                    self.done = true;
                    break;
                }
                wrapped = true;
                self.index = 0;
            }
            let step = self.driver.steps[self.index].clone();
            self.index += 1;
            match &step {
                Step::Wait { wait } => {
                    self.wait_left = wait.max(0.0);
                    if self.wait_left > 0.0 {
                        wrapped = false;
                    }
                    continue;
                }
                Step::Trigger { trigger } => engine.trigger(trigger),
                Step::Set { set } => {
                    for (name, value) in set {
                        engine.set_variable(name, value.clone());
                    }
                }
            }
            applied.push(step);
        }
        applied
    }
}

/// Put the show back to its beginning and walk it to `to` seconds in
/// steps of `1 / fps`, replaying `driver` alongside. Hands back the
/// driver where it ended up, so playing on from there continues.
///
/// A show's state is a function of its inputs and the clock, so reaching
/// a moment is restarting and advancing to it. Nothing is stored, nothing
/// is rewound, and a host can scrub by calling this as the pointer moves:
/// a show of a minute takes a couple of milliseconds.
///
/// The step matters. A chain of timelines linked by `on_end` lands on
/// frame boundaries, so seeking at one rate and playing at another can
/// put them a frame or two apart; use the rate the show plays at.
pub fn seek(
    engine: &mut Engine,
    driver: Option<Driver>,
    to: f64,
    fps: f64,
) -> Option<DriverPlayer> {
    engine.restart();
    let mut player = driver.map(DriverPlayer::new);
    let step = 1.0 / fps.max(1.0);
    let (mut steps, mut time) = (0u64, 0.0_f64);
    while time < to {
        let dt = step.min(to - time);
        if let Some(player) = &mut player {
            player.advance(engine, dt);
        }
        steps += 1;
        // Counted from the start and handed over as the instant to land
        // on, so scrubbing to a moment reaches the state playing to it
        // would, whatever `fps` the walk used.
        time = (steps as f64 * step).min(to);
        engine.advance_to(time);
    }
    player
}
