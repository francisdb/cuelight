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
    /// A frame that arrived before it was due. Held rather than shown, so
    /// the playhead can reach it instead of the stream deciding it has
    /// overshot and seeking back to where it already was.
    early: Option<(u64, Vec<u8>)>,
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
            early: None,
        }
    }

    pub fn details(&self) -> Details {
        self.details
    }

    /// The frame showing `position` seconds in, or the last one still in
    /// hand while the decoder catches up. Never waits.
    pub fn frame_at(&mut self, position: f64) -> Option<&[u8]> {
        let wanted = (position.max(0.0) * self.details.rate).floor() as u64;
        // A frame held back before the playhead jumped away from it, which
        // a loop back to the start does every time round, is no use: drop
        // it, or nothing is ever taken from the channel again and the
        // picture stops until the playhead comes back round to it.
        if self
            .early
            .as_ref()
            .is_some_and(|(at, _)| *at > wanted + SEEK_AFTER)
        {
            self.early = None;
        }
        // One kept back from last time, if the playhead has reached it.
        if self.early.as_ref().is_some_and(|(at, _)| *at <= wanted) {
            let (at, frame) = self.early.take().expect("just checked");
            self.frame = frame;
            self.at = Some(at);
        }
        // Then everything that has arrived, keeping the newest that is due.
        // One that is not due yet is held, not shown: showing it would put
        // the stream ahead of the playhead, and a stream ahead of the
        // playhead used to read that as drift and seek back to a frame it
        // had just decoded, over and over.
        while self.early.is_none() {
            match self.frames.try_recv() {
                Ok((at, frame)) if at <= wanted => {
                    self.frame = frame;
                    self.at = Some(at);
                }
                Ok(early) => self.early = Some(early),
                Err(_) => break,
            }
        }
        // Far from where it should be: ask the decoder to start again
        // there and keep showing what we have meanwhile. Being a little
        // early is ordinary; a loop back to the start is not.
        let adrift = match self.at {
            Some(at) => at > wanted + SEEK_AFTER || wanted > at + SEEK_AFTER,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream with nothing decoding behind it: frames are handed in by
    /// the test, so what is under test is only which one gets shown.
    fn stream(rate: f64) -> (std::sync::mpsc::SyncSender<(u64, Vec<u8>)>, Stream) {
        let (sender, frames) = sync_channel::<(u64, Vec<u8>)>(64);
        let details = Details {
            width: 1,
            height: 1,
            rate,
            duration: 100.0,
        };
        let stream = Stream {
            details,
            frames,
            seek_to: Arc::new(AtomicU64::new(0)),
            seeking: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            frame: Vec::new(),
            at: None,
            early: None,
        };
        (sender, stream)
    }

    /// Which frame is on screen, by the marker the test put in it.
    fn shown(stream: &mut Stream, second: f64) -> Option<u8> {
        stream.frame_at(second).map(|f| f[0])
    }

    #[test]
    fn a_decoder_running_ahead_is_not_mistaken_for_drift() {
        // One frame a second, and a decoder that has run six frames ahead,
        // which is what it is meant to do.
        let (sender, mut stream) = stream(1.0);
        for at in 0..6u64 {
            sender.send((at, vec![at as u8])).unwrap();
        }
        // The playhead walks forward and sees each frame in turn.
        for at in 0..6u64 {
            assert_eq!(shown(&mut stream, at as f64), Some(at as u8), "at {at}");
        }
        assert!(
            !stream.seeking.load(Ordering::Acquire),
            "being early was read as drift, and the decoder was sent back \
             over a frame it had already decoded"
        );
    }

    #[test]
    fn a_frame_that_is_not_due_waits_its_turn() {
        let (sender, mut stream) = stream(1.0);
        sender.send((0, vec![0])).unwrap();
        sender.send((1, vec![1])).unwrap();
        // Half a second in, frame 1 is not due: frame 0 stays on screen.
        assert_eq!(shown(&mut stream, 0.5), Some(0));
        assert_eq!(shown(&mut stream, 0.9), Some(0));
        assert_eq!(shown(&mut stream, 1.0), Some(1), "and arrives on time");
    }

    /// A loop sends the playhead back to the start while a frame from the
    /// end is still held back, unshown. Unless that one is let go, nothing
    /// is ever taken from the channel again and the picture stops for a
    /// whole loop.
    #[test]
    fn a_loop_does_not_leave_the_picture_stuck_on_a_held_frame() {
        let (sender, mut stream) = stream(1.0);
        sender.send((0, vec![0])).unwrap();
        sender.send((40, vec![40])).unwrap();
        assert_eq!(shown(&mut stream, 0.0), Some(0));
        assert!(stream.early.is_some(), "frame 40 is not due yet");

        // Round the loop: the decoder is sent back and starts again.
        sender.send((1, vec![1])).unwrap();
        sender.send((2, vec![2])).unwrap();
        assert_eq!(
            shown(&mut stream, 1.0),
            Some(1),
            "the held frame from the end blocked everything behind it"
        );
        assert_eq!(shown(&mut stream, 2.0), Some(2));
    }

    #[test]
    fn a_playhead_far_from_the_frames_still_seeks() {
        let (sender, mut stream) = stream(1.0);
        sender.send((0, vec![0])).unwrap();
        assert_eq!(shown(&mut stream, 0.0), Some(0));
        assert!(!stream.seeking.load(Ordering::Acquire));

        // A jump well past what is in hand: worth restarting the decoder
        // rather than reading through.
        shown(&mut stream, 0.0 + SEEK_AFTER as f64 + 5.0);
        assert!(stream.seeking.load(Ordering::Acquire));
        assert_eq!(
            stream.seek_to.load(Ordering::Acquire),
            SEEK_AFTER + 5,
            "and it seeks to where the playhead is"
        );
    }
}
