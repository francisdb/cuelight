//! Video for cuelight hosts.
//!
//! The engine decodes nothing. It knows a video by its length and size
//! (`Engine::set_video`), reports which videos should be showing and how
//! far into them (`Engine::videos`), and draws whatever frame the host
//! last handed over through `Engine::set_image`. This crate is that host
//! work: turning a file into frames.
//!
//! ```no_run
//! # fn main() -> Result<(), String> {
//! use cuelight_video::Video;
//!
//! let video = Video::open("intro.mp4", None)?;
//! // engine.set_video("intro", video.duration(), video.size())?;
//! // and per frame, for each playing video the engine reports:
//! // engine.set_image("intro", w, h, video.frame_at(playing.position)?.to_vec())?;
//! # Ok(())
//! # }
//! ```
//!
//! Decoding sits behind cargo features, so a host pays only for what it
//! wants. `ffmpeg-process` (on by default) runs the ffmpeg program and
//! reads raw pixels from it: nothing to build or link, and it plays what
//! ffmpeg plays. Library backends can follow behind the same type.

#[cfg(feature = "ffmpeg-process")]
mod ffmpeg;
#[cfg(feature = "ffmpeg-process")]
mod stream;

#[cfg(feature = "ffmpeg-process")]
pub use stream::Stream;

/// A clip's own soundtrack, as interleaved stereo samples at `rate`, or
/// `None` when the file has no audio.
///
/// Takes a path rather than a [`Clip`] so a host can decode a folder's
/// worth at once: a clip holds a decoder and is not shareable between
/// threads, and this needs nothing but the file.
#[cfg(feature = "ffmpeg-process")]
pub fn soundtrack(
    path: impl AsRef<std::path::Path>,
    rate: u32,
) -> Result<Option<Vec<f32>>, String> {
    ffmpeg::soundtrack(path.as_ref(), rate)
}

/// What a video is, before any of it is decoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Details {
    pub width: u32,
    pub height: u32,
    /// Frames per second it will be decoded at.
    pub rate: f64,
    /// Seconds it runs for.
    pub duration: f64,
}

impl Details {
    /// Bytes one frame takes as straight RGBA.
    pub fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// How many frames it holds.
    pub fn frames(&self) -> u64 {
        (self.duration * self.rate).round() as u64
    }

    /// What holding every frame would cost.
    pub fn bytes_in_full(&self) -> u64 {
        self.frames() * self.frame_bytes() as u64
    }

    pub fn size(&self) -> [f64; 2] {
        [f64::from(self.width), f64::from(self.height)]
    }
}

/// A clip a host can ask for frames, however it is being decoded.
///
/// Opening one only measures it. Nothing is decoded until a frame is
/// asked for, which is what makes a folder of clips openable at all: a
/// show can name hundreds and play three, and the ones nothing watches
/// cost a probe each. On that first frame the clip decides for itself: a
/// short one is decoded once and kept, which costs memory but never
/// stutters, and anything larger is decoded as it plays, which costs a
/// handful of frames whatever its length (see [`Decode::budget`]).
/// Either way the decoding happens on a thread of its own, so asking for
/// a frame never holds up a show; until the first one arrives the clip
/// has nothing to show, exactly as before a host registers an image.
///
/// [`Clip::rest`] gives back what a clip is using when nothing is
/// watching it any more.
pub struct Clip {
    details: Details,
    how: Decode,
    path: std::path::PathBuf,
    source: Source,
}

/// What a clip is reading frames out of, once something has asked.
enum Source {
    /// Nothing yet: measured, not decoded.
    Idle,
    /// Being decoded whole, on a thread of its own. Decoding a clip takes
    /// long enough to be seen as a freeze, and a frame of the show is not
    /// the place to do it: the layer shows nothing for the moment it
    /// takes, and the show keeps its pace.
    #[cfg(feature = "ffmpeg-process")]
    Decoding(std::thread::JoinHandle<Result<Video, String>>),
    /// Every frame, in memory.
    Whole(Video),
    /// One frame at a time, as it plays.
    #[cfg(feature = "ffmpeg-process")]
    Streamed(Stream),
}

impl Clip {
    /// Measure the video at `path`. This reads the file's header, not the
    /// video: decoding waits for the first [`Clip::frame_at`].
    pub fn open(path: impl AsRef<std::path::Path>, how: Option<Decode>) -> Result<Clip, String> {
        let how = how.unwrap_or_default();
        let path = path.as_ref();
        #[cfg(feature = "ffmpeg-process")]
        {
            Ok(Clip {
                details: ffmpeg::details(path, how)?,
                how,
                path: path.to_owned(),
                source: Source::Idle,
            })
        }
        #[cfg(not(feature = "ffmpeg-process"))]
        {
            let _ = (path, how);
            Err("no video decoder is compiled in".into())
        }
    }

    /// A clip out of frames already in hand, decoded elsewhere.
    pub fn held(video: Video) -> Clip {
        Clip {
            details: video.details(),
            how: Decode::default(),
            path: std::path::PathBuf::new(),
            source: Source::Whole(video),
        }
    }

    pub fn details(&self) -> Details {
        self.details
    }

    /// Whether the clip is decoding, or has decoded, anything.
    pub fn is_decoding(&self) -> bool {
        !matches!(self.source, Source::Idle)
    }

    /// Start decoding without waiting for a frame to be asked for: what a
    /// host calls when it knows what is about to play.
    pub fn warm(&mut self) {
        if matches!(self.source, Source::Idle) {
            self.source = self.start();
        }
    }

    /// The frame showing `position` seconds in. The first call starts the
    /// clip decoding, so it may take as long as a frame takes to arrive.
    pub fn frame_at(&mut self, position: f64) -> Option<&[u8]> {
        self.warm();
        // Decoded by now? Take delivery; the frames are wanted right away.
        #[cfg(feature = "ffmpeg-process")]
        if matches!(&self.source, Source::Decoding(work) if work.is_finished()) {
            let Source::Decoding(work) = std::mem::replace(&mut self.source, Source::Idle) else {
                unreachable!("just matched")
            };
            self.source = match work.join() {
                Ok(Ok(video)) => Source::Whole(video),
                Ok(Err(e)) => {
                    log::warn!("{:?}: {e}", self.path);
                    Source::Idle
                }
                Err(_) => {
                    log::warn!("{:?}: decoding gave up", self.path);
                    Source::Idle
                }
            };
        }
        match &mut self.source {
            // Nothing to show yet, as before a host registers an image.
            Source::Idle => None,
            Source::Whole(video) => video.frame_at(position),
            #[cfg(feature = "ffmpeg-process")]
            Source::Decoding(_) => None,
            #[cfg(feature = "ffmpeg-process")]
            Source::Streamed(stream) => stream.frame_at(position),
        }
    }

    /// Give back what the clip is using; it starts again on the next
    /// frame asked for. A clip being decoded as it plays holds a decoder
    /// open, which is worth stopping the moment nothing is watching. One
    /// held in memory keeps its frames: they fit the budget, and throwing
    /// them away only means decoding them again.
    pub fn rest(&mut self) {
        #[cfg(feature = "ffmpeg-process")]
        if matches!(self.source, Source::Streamed(_)) {
            self.source = Source::Idle;
        }
    }

    /// The clip's own soundtrack, as interleaved stereo samples at
    /// `rate`, or `None` when the file has no audio.
    ///
    /// A clip is heard by being registered as a sound under the video's
    /// name, so a host hands these to the engine's `set_sound` and to
    /// whatever mixes for it; the engine then reports the layer among its
    /// voices for as long as the picture plays. A host that wants a clip
    /// seen and not heard simply does not ask for this.
    ///
    /// Unlike the pictures, this is decoded in full when asked: a mixer
    /// needs samples in hand, not a stream. Minutes of audio are tens of
    /// megabytes, so ask per clip rather than for a whole folder.
    #[cfg(feature = "ffmpeg-process")]
    pub fn soundtrack(&self, rate: u32) -> Result<Option<Vec<f32>>, String> {
        soundtrack(&self.path, rate)
    }

    /// Whether the clip is still being decoded and has nothing to show.
    pub fn is_warming(&self) -> bool {
        #[cfg(feature = "ffmpeg-process")]
        return matches!(self.source, Source::Decoding(_));
        #[cfg(not(feature = "ffmpeg-process"))]
        false
    }

    /// Pick a way to read this clip and open it.
    fn start(&self) -> Source {
        #[cfg(feature = "ffmpeg-process")]
        {
            let megabytes = self.details.bytes_in_full() / (1024 * 1024);
            if self.details.bytes_in_full() <= self.how.budget {
                log::debug!(
                    "{:?}: {megabytes} MB of frames, holding all of it",
                    self.path
                );
                let (path, how) = (self.path.clone(), self.how);
                return match std::thread::Builder::new()
                    .name(format!("cuelight-video {}", path.display()))
                    .spawn(move || Video::open(&path, Some(how)))
                {
                    Ok(work) => Source::Decoding(work),
                    Err(e) => {
                        log::warn!("{:?}: {e}", self.path);
                        Source::Idle
                    }
                };
            }
            log::debug!(
                "{:?}: {megabytes} MB of frames, decoding as it plays",
                self.path
            );
            Source::Streamed(Stream::open(&self.path, self.details, self.how))
        }
        #[cfg(not(feature = "ffmpeg-process"))]
        {
            // Nothing to start it with; a clip built by hand holds its
            // frames already.
            let _ = (&self.path, &self.how);
            Source::Idle
        }
    }
}

/// A decoded video: every frame in memory, as straight RGBA.
///
/// Holding a whole clip suits the short ones a show uses. A streaming
/// source would suit a long one, and the engine cannot tell the
/// difference: all it ever says is which video should be showing and how
/// far into it.
#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    width: u32,
    height: u32,
    /// Frames per second the frames were decoded at.
    rate: f64,
    frames: Vec<Vec<u8>>,
}

/// How a video should be decoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decode {
    /// Frames per second to decode at; the source's own rate when `None`.
    /// A show rarely needs more than the display shows.
    pub rate: Option<f64>,
    /// The largest size to decode at; the source's own when `None`. A
    /// clip is never scaled up, and its shape is kept, so this is a
    /// bound rather than a size. A backglass video is often far larger
    /// than the canvas it plays on, and frames cost by the pixel.
    pub size: Option<[u32; 2]>,
    /// How much memory a clip may take before [`Clip::open`] decodes it
    /// as it plays instead of holding it. 32 MB by default, which keeps
    /// short loops in memory, where they never stutter, and streams the
    /// rest.
    pub budget: u64,
}

impl Default for Decode {
    fn default() -> Self {
        Self {
            rate: Some(30.0),
            size: None,
            budget: 32 * 1024 * 1024,
        }
    }
}

impl Video {
    /// Decode the video file at `path` into frames. `None` decodes at 30
    /// frames a second and the video's own size.
    ///
    /// A file is the sure way in: some containers, MP4 among them, keep
    /// their index at the end and cannot be read as a stream at all.
    pub fn open(path: impl AsRef<std::path::Path>, how: Option<Decode>) -> Result<Video, String> {
        let how = how.unwrap_or_default();
        #[cfg(feature = "ffmpeg-process")]
        {
            ffmpeg::decode(ffmpeg::Source::File(path.as_ref()), how)
        }
        #[cfg(not(feature = "ffmpeg-process"))]
        {
            let _ = (path, how);
            Err("no video decoder is compiled in".into())
        }
    }

    /// Decode `bytes` into frames, for a host holding a video in memory.
    /// A container that keeps its index at the end, as MP4 does, cannot
    /// be read this way: see [`Video::open`].
    pub fn decode(bytes: &[u8], how: Option<Decode>) -> Result<Video, String> {
        let how = how.unwrap_or_default();
        #[cfg(feature = "ffmpeg-process")]
        {
            ffmpeg::decode(ffmpeg::Source::Bytes(bytes), how)
        }
        #[cfg(not(feature = "ffmpeg-process"))]
        {
            let _ = (bytes, how);
            Err("no video decoder is compiled in".into())
        }
    }

    /// Build one from frames already in hand, for hosts that decode
    /// elsewhere and for tests.
    pub fn from_frames(
        [width, height]: [u32; 2],
        rate: f64,
        frames: Vec<Vec<u8>>,
    ) -> Result<Video, String> {
        let wanted = width as usize * height as usize * 4;
        if width == 0 || height == 0 || rate <= 0.0 || rate.is_nan() {
            return Err(format!("{width}x{height} at {rate} fps is not a video"));
        }
        if let Some(bad) = frames.iter().find(|frame| frame.len() != wanted) {
            return Err(format!(
                "a frame of {} bytes, expected {wanted} for {width}x{height}",
                bad.len()
            ));
        }
        Ok(Video {
            width,
            height,
            rate,
            frames,
        })
    }

    /// Seconds the video runs for, what the engine registers.
    pub fn duration(&self) -> f64 {
        self.frames.len() as f64 / self.rate
    }

    /// What it is, before asking for any of it.
    pub fn details(&self) -> Details {
        Details {
            width: self.width,
            height: self.height,
            rate: self.rate,
            duration: self.duration(),
        }
    }

    /// Its size in pixels, what the engine lays a layer out with.
    pub fn size(&self) -> [f64; 2] {
        [f64::from(self.width), f64::from(self.height)]
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn frames(&self) -> usize {
        self.frames.len()
    }

    /// The frame showing `position` seconds in; the last one after the
    /// end, `None` while there are no frames at all.
    pub fn frame_at(&self, position: f64) -> Option<&[u8]> {
        if self.frames.is_empty() {
            return None;
        }
        let index = (position.max(0.0) * self.rate).floor() as usize;
        self.frames
            .get(index.min(self.frames.len() - 1))
            .map(Vec::as_slice)
    }
}
