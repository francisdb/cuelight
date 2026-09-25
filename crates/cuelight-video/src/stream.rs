//! Decoding a clip while it plays, on a thread of its own: ffmpeg writes
//! frames, a worker reads a few ahead, and whoever is drawing takes the
//! newest one that has arrived without ever waiting. A long clip then
//! costs a handful of frames of memory instead of all of them, and a seek
//! or a loop holds the last picture for a moment rather than stopping
//! everything.

use crate::{Decode, Details, Kept};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::sync::Arc;
use std::time::Duration;

/// How much decoding is kept ahead of the playhead, in seconds. Enough
/// to smooth over a slow frame without holding much: at 640x360 a frame
/// is under a megabyte, and a third of a second of them is a handful
/// whatever the clip's rate. Counted in time rather than frames, so a
/// 60 fps clip absorbs the same hiccup a 30 fps one does.
const AHEAD: f64 = 0.3;

/// How far behind the playhead may fall before it is worth seeking rather
/// than reading through, in frames.
const SEEK_AFTER: u64 = 30;

/// Seconds of frames arriving on time that end a stretch of late ones,
/// so one stutter is one line in the log however it flickers.
const QUIET: f64 = 0.25;

/// Late frames a stretch has to reach before it is worth a warning.
/// Every clip is a frame or two late as its decoder starts, and that
/// once per play would bury the stutters worth seeing; shorter stretches
/// are still counted, and still said at `debug`.
const A_STUTTER: u64 = 3;

/// A clip decoded as it is watched.
pub struct Stream {
    details: Details,
    /// What to call the clip when something is worth saying about it.
    name: String,
    /// Frames in one round of the clip. A looping playhead comes back to
    /// the start, and the frames keep counting, so both are read as one
    /// number that only ever grows.
    span: u64,
    frames: Receiver<(u64, Vec<u8>)>,
    /// Where the worker should start decoding from, when it differs from
    /// where it is.
    seek_to: Arc<AtomicU64>,
    seeking: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// Whether the play loops, as the host says; see
    /// [`Stream::set_looping`].
    looping: Arc<AtomicBool>,
    /// Decoders started, which the worker counts: one to begin with, and
    /// one more for every seek and every end of a clip read again.
    starts: Arc<AtomicU64>,
    /// The picture in hand, and which frame it is.
    frame: Vec<u8>,
    at: Option<u64>,
    /// A frame that arrived before it was due. Held rather than shown, so
    /// the playhead can reach it instead of the stream deciding it has
    /// overshot and seeking back to where it already was.
    early: Option<(u64, Vec<u8>)>,
    /// How many times the playhead has been round a looping clip, and
    /// where it was last, so going round again can be told from a seek
    /// backwards.
    round: u64,
    last: Option<u64>,
    kept: Kept,
    /// The last frame of the clip that was judged late or on time, so
    /// each one counts once however often the host draws.
    counted: Option<u64>,
    /// A stretch of late frames being counted up. Reported when it ends,
    /// so a stutter is one line rather than one per frame.
    behind: Option<Behind>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // A stretch that was still running when the clip stopped being
        // watched is still worth a line.
        if let Some(behind) = self.behind {
            self.report(behind);
        }
    }
}

impl Stream {
    pub(crate) fn open(path: &Path, details: Details, _how: Decode) -> Stream {
        let span = span_of(details);
        let (sender, frames) = sync_channel::<(u64, Vec<u8>)>(ahead(details.rate));
        let seek_to = Arc::new(AtomicU64::new(0));
        let seeking = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let looping = Arc::new(AtomicBool::new(false));
        let starts = Arc::new(AtomicU64::new(0));
        let worker = Worker {
            path: path.to_owned(),
            details,
            span,
            looping: looping.clone(),
            starts: starts.clone(),
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
            name: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            span,
            frames,
            seek_to,
            seeking,
            stop,
            looping,
            starts,
            frame: Vec::new(),
            at: None,
            early: None,
            round: 0,
            last: None,
            kept: Kept::default(),
            counted: None,
            behind: None,
        }
    }

    pub fn details(&self) -> Details {
        self.details
    }

    /// What it has had to do to keep up so far.
    pub fn kept(&self) -> Kept {
        Kept {
            starts: self.starts.load(Ordering::Relaxed),
            ..self.kept
        }
    }

    /// Say whether the play loops, which the host knows and the clip
    /// cannot. A decoder started from the top while this holds runs the
    /// clip round and round itself, so going round costs nothing: it is
    /// read when a decoder starts, and a change reaches the next one.
    pub fn set_looping(&self, looping: bool) {
        self.looping.store(looping, Ordering::Release);
    }

    /// The frame showing `position` seconds in, or the last one still in
    /// hand while the decoder catches up. Never waits.
    ///
    /// `show` is the host's clock, used only to say when a stretch of
    /// late frames happened: a position in a looping clip comes round
    /// again and does not place it.
    pub fn frame_at(&mut self, position: f64, show: Option<f64>) -> Option<&[u8]> {
        let inside = (position.max(0.0) * self.details.rate).floor() as u64;
        let wanted = self.counting(inside);
        // A frame held back before the playhead jumped away from it is no
        // use: drop it, or nothing is ever taken from the channel again
        // and the picture stops until the playhead comes back to it.
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
                Ok((at, frame)) => {
                    let at = self.round_of(at, wanted);
                    match at <= wanted {
                        true => {
                            self.frame = frame;
                            self.at = Some(at);
                        }
                        false => self.early = Some((at, frame)),
                    }
                }
                Err(_) => break,
            }
        }
        self.count_late(wanted, position, show);
        // Far from where it should be: ask the decoder to start again
        // there and keep showing what we have meanwhile. Being a little
        // early is ordinary; going round a loop is not a jump at all,
        // since both sides keep counting past the end of the clip.
        let adrift = match self.at {
            Some(at) => at > wanted + SEEK_AFTER || wanted > at + SEEK_AFTER,
            None => false,
        };
        if adrift && !self.seeking.swap(true, Ordering::AcqRel) {
            self.kept.seeks += 1;
            self.seek_to.store(inside, Ordering::Release);
            log::debug!("{:?}: seeking to frame {inside}", self.name);
        }
        (!self.frame.is_empty()).then_some(self.frame.as_slice())
    }

    /// The playhead's frame as a number that keeps counting past the end
    /// of the clip, so a loop back to the start is not a jump backwards.
    ///
    /// A playhead that went back by more than half the clip has been
    /// round again; anything less is a seek, and stays in this round.
    fn counting(&mut self, inside: u64) -> u64 {
        let half = self.span / 2;
        if let Some(last) = self.last {
            if last > inside && last - inside > half.max(1) {
                self.round += 1;
            }
        }
        self.last = Some(inside);
        self.round * self.span + inside
    }

    /// A decoded frame's number, counted the same way: the round that
    /// puts it nearest the playhead, since the decoder counts within the
    /// clip and may be a frame either side of where the playhead thinks
    /// the end is.
    fn round_of(&self, at: u64, wanted: u64) -> u64 {
        let span = self.span as i64;
        let (at, wanted) = (at as i64, wanted as i64);
        let rounds = ((wanted - at) as f64 / span as f64).round() as i64;
        (at + rounds * span).max(0) as u64
    }

    /// Count a frame drawn older than the one that was due, and report a
    /// stretch of them once it is over: a clip that stutters says so
    /// once for the stutter, not once a frame. A frame or two arriving
    /// on time does not end a stretch, or a stutter would be reported a
    /// line at a time.
    fn count_late(&mut self, wanted: u64, position: f64, show: Option<f64>) {
        // Once per frame of the clip, not once per frame the host draws:
        // a host drawing at twice the clip's rate is not twice as late,
        // and the count means frames of the clip that never made it.
        if self.counted == Some(wanted) {
            return;
        }
        self.counted = Some(wanted);
        if self.at.is_some_and(|at| at < wanted) {
            self.kept.late += 1;
            self.behind = Some(match self.behind {
                None => Behind {
                    frames: 1,
                    from: position,
                    to: position,
                    show,
                },
                Some(behind) => Behind {
                    frames: behind.frames + 1,
                    to: position,
                    ..behind
                },
            });
            return;
        }
        let Some(behind) = self.behind else {
            return;
        };
        if position - behind.to > QUIET {
            self.behind = None;
            self.report(behind);
        }
    }

    /// Say that a stretch of frames was late.
    fn report(&self, behind: Behind) {
        let (line, loud) = late_line(&self.name, behind);
        // A frame or two as a decoder starts is every clip, every play,
        // and saying so out loud would bury the stutters worth seeing.
        match loud {
            true => log::warn!("{line}"),
            false => log::debug!("{line}"),
        }
    }
}

/// What ffmpeg is told before the input: where to start, and whether to
/// run the clip round and round by itself.
///
/// A play that loops is decoded by one process that never reaches an
/// end, so going round costs nothing: no restart, and the file is not
/// opened again. That only works from the top, since a seek before the
/// input is where every round would start, so a decoder sent part way
/// into a clip reads it once and rounds to the start when it gets
/// there.
fn decoding(details: &Details, frame: u64, looping: bool) -> Vec<String> {
    let mut args = vec!["-v".to_owned(), "error".to_owned()];
    let seconds = frame as f64 / details.rate;
    if seconds > 0.0 {
        // Before the input, so ffmpeg seeks rather than decoding its way
        // there.
        args.extend(["-ss".to_owned(), format!("{seconds:.3}")]);
    } else if looping {
        args.extend(["-stream_loop".to_owned(), "-1".to_owned()]);
    }
    args
}

/// What ffmpeg is told after the input: raw frames of this size and
/// rate, down the pipe.
fn ending(details: &Details) -> Vec<String> {
    vec![
        "-f".to_owned(),
        "rawvideo".to_owned(),
        "-pix_fmt".to_owned(),
        "rgba".to_owned(),
        "-s".to_owned(),
        format!("{}x{}", details.width, details.height),
        "-r".to_owned(),
        details.rate.to_string(),
        "pipe:1".to_owned(),
    ]
}

/// What to say about a stretch of late frames, and whether it is worth
/// a warning.
///
/// Where in the clip it was, and when in the show that was: a looping
/// clip is at the same position every round, so the clip's own clock
/// does not place it.
fn late_line(name: &str, behind: Behind) -> (String, bool) {
    let Behind {
        frames,
        from,
        to,
        show,
    } = behind;
    let where_in_the_clip = match frames {
        1 => format!("1 frame late at {from:.1}s of the clip"),
        n => format!("{n} frames late between {from:.1}s and {to:.1}s of the clip"),
    };
    let when = match show {
        Some(show) => format!(" (show {show:.1}s)"),
        None => String::new(),
    };
    (
        format!("{name:?}: {where_in_the_clip}{when}: the decoder was not there yet"),
        frames > A_STUTTER,
    )
}

/// A stretch of frames drawn later than they should have been: how
/// many, where in the clip it started and last ran to, and the host's
/// clock when it started.
#[derive(Debug, Clone, Copy)]
struct Behind {
    frames: u64,
    from: f64,
    to: f64,
    show: Option<f64>,
}

/// Frames to keep decoded ahead of the playhead, at this rate.
fn ahead(rate: f64) -> usize {
    match (AHEAD * rate).ceil() {
        frames if frames.is_finite() => (frames as usize).max(2),
        _ => 2,
    }
}

/// Frames in one round of a clip.
fn span_of(details: Details) -> u64 {
    (details.duration * details.rate).round().max(1.0) as u64
}

struct Worker {
    path: PathBuf,
    details: Details,
    /// Frames in one round, so the end of the clip can run straight into
    /// its start again.
    span: u64,
    /// Whether the play loops, as the host last said. Read when a
    /// decoder starts: one told to loop runs the clip round and round
    /// itself and never reaches an end.
    looping: Arc<AtomicBool>,
    /// Decoders started, for a host that wants to know how often.
    starts: Arc<AtomicU64>,
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
                next = self.seek_to.load(Ordering::Acquire) % self.span.max(1);
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
                // The end of the clip. A decoder told to loop does not
                // reach one, so this is either a play of one round or a
                // decoder that was started part way in, before anyone
                // said it loops: round to the start and carry on, which
                // is the last restart a looping play pays for. A play
                // that does not loop asks for none of those frames, and
                // the decoder blocks on a queue nobody drains.
                next = 0;
                decoder = self.start(0);
                if decoder.is_none() {
                    std::thread::sleep(Duration::from_millis(50));
                }
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
        let looping = self.looping.load(Ordering::Acquire);
        self.starts.fetch_add(1, Ordering::Relaxed);
        log::debug!(
            "{:?}: starting a decoder at frame {frame}{}",
            self.path,
            match looping {
                true => ", to run round and round",
                false => "",
            }
        );
        let mut command = Command::new("ffmpeg");
        command.args(decoding(&self.details, frame, looping));
        let mut child = command
            .arg("-i")
            .arg(&self.path)
            .args(ending(&self.details))
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
            looping: Arc::new(AtomicBool::new(false)),
            starts: Arc::new(AtomicU64::new(0)),
            name: "test".to_owned(),
            span: span_of(details),
            frames,
            seek_to: Arc::new(AtomicU64::new(0)),
            seeking: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            frame: Vec::new(),
            at: None,
            early: None,
            round: 0,
            last: None,
            kept: Kept::default(),
            counted: None,
            behind: None,
        };
        (sender, stream)
    }

    /// Which frame is on screen, by the marker the test put in it.
    fn shown(stream: &mut Stream, second: f64) -> Option<u8> {
        stream.frame_at(second, None).map(|f| f[0])
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
    fn a_looping_clip_is_decoded_by_one_process_that_never_ends() {
        let details = Details {
            width: 16,
            height: 8,
            rate: 30.0,
            duration: 50.0,
        };
        // From the top, told it loops: ffmpeg runs the clip round and
        // round itself, so a wrap costs no restart at all.
        let looping = decoding(&details, 0, true);
        assert!(
            looping.windows(2).any(|w| w == ["-stream_loop", "-1"]),
            "{looping:?}"
        );
        assert!(!looping.iter().any(|arg| arg == "-ss"), "{looping:?}");
        // A play of one round is read once, as before.
        assert_eq!(decoding(&details, 0, false), ["-v", "error"]);
        // Part way in there is no looping to ask for: a seek before the
        // input is where every round would start from, so the clip
        // would come round to the wrong place.
        let sought = decoding(&details, 90, true);
        assert!(
            sought.windows(2).any(|w| w == ["-ss", "3.000"]),
            "{sought:?}"
        );
        assert!(
            !sought.iter().any(|arg| arg == "-stream_loop"),
            "{sought:?}"
        );
    }

    #[test]
    fn a_loop_runs_on_rather_than_seeking_back() {
        // The test stream is a hundred frames long, one a second. The
        // decoder has run past the end into the start again, which is
        // what it does for a clip that loops.
        let (sender, mut stream) = stream(1.0);
        for at in 97..100u64 {
            sender.send((at, vec![at as u8])).unwrap();
        }
        for at in 0..3u64 {
            sender.send((at, vec![200 + at as u8])).unwrap();
        }
        for at in 97..100u64 {
            assert_eq!(shown(&mut stream, at as f64), Some(at as u8), "at {at}");
        }
        // Round again: the frames from the next round were held back
        // until the playhead reached them, and are there when it does.
        assert_eq!(shown(&mut stream, 0.0), Some(200), "the first of the round");
        assert_eq!(shown(&mut stream, 1.0), Some(201));
        assert_eq!(shown(&mut stream, 2.0), Some(202));
        assert!(
            !stream.seeking.load(Ordering::Acquire),
            "going round was read as drift, and the decoder was sent back \
             over frames it had already decoded"
        );
        assert_eq!(
            stream.kept(),
            Kept::default(),
            "nothing late, no seeks, no decoder started again"
        );
    }

    #[test]
    fn a_stretch_says_where_in_the_clip_and_when_in_the_show() {
        let behind = |frames, from, to, show| Behind {
            frames,
            from,
            to,
            show,
        };
        // Every clip is a frame or two late as its decoder starts: said
        // quietly, and as one moment rather than a span.
        assert_eq!(
            late_line("loading", behind(1, 0.1, 0.1, Some(41.2))),
            (
                "\"loading\": 1 frame late at 0.1s of the clip (show 41.2s): \
                 the decoder was not there yet"
                    .to_owned(),
                false
            )
        );
        // A stutter is worth saying out loud, and the show's clock
        // places it: the clip's own comes round again every loop.
        assert_eq!(
            late_line("loading", behind(130, 0.5, 2.6, Some(41.2))),
            (
                "\"loading\": 130 frames late between 0.5s and 2.6s of the clip \
                 (show 41.2s): the decoder was not there yet"
                    .to_owned(),
                true
            )
        );
        // A host with no clock of its own says nothing about one.
        let (line, _) = late_line("loading", behind(9, 0.5, 0.9, None));
        assert!(!line.contains("show"), "{line}");
    }

    #[test]
    fn frames_the_decoder_did_not_reach_are_counted_once_the_stutter_ends() {
        let (sender, mut stream) = stream(1.0);
        sender.send((0, vec![0])).unwrap();
        assert_eq!(shown(&mut stream, 0.0), Some(0));
        // Three seconds with nothing new: the picture holds, which is
        // the right thing to draw and worth saying out loud.
        for second in 1..4 {
            assert_eq!(shown(&mut stream, f64::from(second)), Some(0));
        }
        assert_eq!(stream.kept().late, 3, "counted as they happen");
        sender.send((4, vec![4])).unwrap();
        assert_eq!(shown(&mut stream, 4.0), Some(4));
        assert_eq!(stream.kept().late, 3, "and the picture is back");
        assert_eq!(stream.kept().seeks, 0);
    }

    #[test]
    fn the_queue_holds_the_same_time_at_any_rate() {
        // A third of a second of frames, so a clip at twice the rate is
        // not smoothed over half as long a hiccup.
        assert_eq!(ahead(30.0), 9);
        assert_eq!(ahead(60.0), 18);
        assert_eq!(ahead(0.0), 2, "never nothing");
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
