//! Decoding a clip while it plays, on a thread of its own: ffmpeg writes
//! frames, a worker reads a few ahead, and whoever is drawing takes the
//! newest one that has arrived without ever waiting. A long clip then
//! costs a handful of frames of memory instead of all of them, and a seek
//! or a loop holds the last picture for a moment rather than stopping
//! everything.

use crate::{Decode, Details};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::sync::Arc;
use std::time::Duration;

/// Frames kept ahead of the playhead. A few smooth over a slow frame
/// without holding much: at 640x360 each is under a megabyte.
const AHEAD: usize = 6;

/// How far behind the playhead may fall before it is worth seeking rather
/// than reading through, in frames.
const SEEK_AFTER: u64 = 30;

/// A clip decoded as it is watched.
pub struct Stream {
    details: Details,
    frames: Receiver<(u64, Vec<u8>)>,
    /// Where the worker should start decoding from, when it differs from
    /// where it is.
    seek_to: Arc<AtomicU64>,
    seeking: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// The picture in hand, and which frame it is.
    frame: Vec<u8>,
    at: Option<u64>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Stream {
    pub(crate) fn open(path: &Path, details: Details, _how: Decode) -> Stream {
        let (sender, frames) = sync_channel::<(u64, Vec<u8>)>(AHEAD);
        let seek_to = Arc::new(AtomicU64::new(0));
        let seeking = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            path: path.to_owned(),
            details,
            seek_to: seek_to.clone(),
            seeking: seeking.clone(),
            stop: stop.clone(),
        };
        std::thread::Builder::new()
            .name(format!("cuelight-video {}", path.display()))
            .spawn(move || worker.run(&sender))
            .map_err(|e| log::warn!("no decoding thread: {e}"))
            .ok();
        Stream {
            details,
            frames,
            seek_to,
            seeking,
            stop,
            frame: Vec::new(),
            at: None,
        }
    }

    pub fn details(&self) -> Details {
        self.details
    }

    /// The frame showing `position` seconds in, or the last one still in
    /// hand while the decoder catches up. Never waits.
    pub fn frame_at(&mut self, position: f64) -> Option<&[u8]> {
        let wanted = (position.max(0.0) * self.details.rate).floor() as u64;
        // Take everything that has arrived, keeping the newest that is not
        // past what is wanted.
        while let Ok((at, frame)) = self.frames.try_recv() {
            let behind_or_at = at <= wanted;
            self.frame = frame;
            self.at = Some(at);
            if !behind_or_at {
                break;
            }
        }
        // Behind, ahead, or looped: ask the decoder to start again there
        // and keep showing what we have meanwhile.
        let adrift = match self.at {
            Some(at) => wanted < at || wanted - at > SEEK_AFTER,
            None => false,
        };
        if adrift && !self.seeking.swap(true, Ordering::AcqRel) {
            self.seek_to.store(wanted, Ordering::Release);
            log::trace!("seeking to frame {wanted}");
        }
        (!self.frame.is_empty()).then_some(self.frame.as_slice())
    }
}

struct Worker {
    path: PathBuf,
    details: Details,
    seek_to: Arc<AtomicU64>,
    seeking: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Worker {
    /// Decode for as long as anyone is listening.
    fn run(self, sender: &std::sync::mpsc::SyncSender<(u64, Vec<u8>)>) {
        let size = self.details.frame_bytes();
        let (mut decoder, mut next) = (None, 0u64);
        let mut frame = vec![0u8; size];
        while !self.stop.load(Ordering::Relaxed) {
            // A seek was asked for, or nothing is running yet.
            if self.seeking.load(Ordering::Acquire) || decoder.is_none() {
                next = self.seek_to.load(Ordering::Acquire);
                decoder = self.start(next);
                self.seeking.store(false, Ordering::Release);
                if decoder.is_none() {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            }
            let Some(running) = &mut decoder else {
                continue;
            };
            if running.read(&mut frame).is_err() {
                // The end of the clip: wait to be sent somewhere else.
                decoder = None;
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            // Hand it over, minding a listener that is not keeping up and
            // a seek that arrives while we wait for room.
            let mut ready = (next, frame.clone());
            loop {
                match sender.try_send(ready) {
                    Ok(()) => break,
                    Err(TrySendError::Full(held)) => {
                        if self.stop.load(Ordering::Relaxed) || self.seeking.load(Ordering::Acquire)
                        {
                            break;
                        }
                        ready = held;
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    // Nobody is listening any more.
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
            next += 1;
        }
    }

    /// Start ffmpeg at `frame`.
    fn start(&self, frame: u64) -> Option<Decoder> {
        let seconds = frame as f64 / self.details.rate;
        let mut command = Command::new("ffmpeg");
        command.args(["-v", "error"]);
        // Before the input, so ffmpeg seeks rather than decoding its way
        // there.
        if seconds > 0.0 {
            command.args(["-ss", &format!("{seconds:.3}")]);
        }
        let mut child = command
            .arg("-i")
            .arg(&self.path)
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-s"])
            .arg(format!("{}x{}", self.details.width, self.details.height))
            .args(["-r", &self.details.rate.to_string(), "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| log::warn!("ffmpeg did not run ({e}); is it installed?"))
            .ok()?;
        let stdout = child.stdout.take()?;
        Some(Decoder {
            child,
            stdout: Box::new(stdout),
        })
    }
}

/// An ffmpeg writing frames.
struct Decoder {
    child: Child,
    stdout: Box<dyn Read + Send>,
}

impl Decoder {
    fn read(&mut self, frame: &mut [u8]) -> std::io::Result<()> {
        self.stdout.read_exact(frame)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
