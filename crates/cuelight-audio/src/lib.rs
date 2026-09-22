//! Sound for [`cuelight`] hosts.
//!
//! The engine never touches samples: it registers sounds by duration and
//! reports what should be heard as a list of [`Voice`]s
//! ([`Engine::voices`](cuelight::Engine::voices)), one per playing sound
//! with its position and gain. This crate is the other half:
//!
//! - [`Sound`]: a decoded sound file (WAV, FLAC, Ogg Vorbis, MP3), which
//!   also gives the duration the engine needs.
//! - [`Mixer`]: plays a voice list. [`Mixer::apply`] takes the list each
//!   frame and starts, stops, retunes and resyncs its voices to match
//!   (only on a seek: a voice that merely runs behind the engine, as it
//!   does when the sound device was asleep when the sound started, keeps
//!   the start of its sound rather than skipping to the middle);
//!   [`Mixer::render`] fills a buffer of interleaved stereo samples.
//!   Called in a plain loop it renders a show's sound offline, sample-exact
//!   against the frames ([`write_wav`] saves it).
//! - [`Output`] (feature `live`, on by default): a mixer on a sound
//!   device's own thread, fed the voice list through a queue. The device
//!   is opened when it is built and held. Whether to build one at all is
//!   the host's call: a show that cannot make a sound (`Show::has_sound`)
//!   never gets an `Output`, so it is never listed by a desktop as an
//!   application making one, and a show that can has its device awake
//!   before the first cue rather than waking a sleeping sound card in
//!   front of it.
//!
//! A host that has a mixer of its own can consume the voice list directly
//! and skip all of this.

mod mixer;
mod sound;
mod wav;

#[cfg(feature = "live")]
mod live;

pub use cuelight::Voice;
#[cfg(feature = "live")]
pub use live::Output;
pub use mixer::Mixer;
pub use sound::{Sound, SOUND_EXTENSIONS};
pub use wav::write_wav;
