//! Decoding by running ffmpeg as a program: it writes raw pixels, we read
//! them. Nothing is linked and nothing is built, so a host needs only the
//! ffmpeg most machines already have, and it plays whatever ffmpeg plays.

use crate::{Decode, Video};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Where the video is: a file ffmpeg can open and seek in, or bytes fed
/// to it. A container that keeps its index at the end, as MP4 does,
/// cannot be read from a pipe at all, so a file is the sure way.
pub enum Source<'a> {
    File(&'a Path),
    Bytes(&'a [u8]),
}

impl Source<'_> {
    fn input(&self) -> &std::ffi::OsStr {
        match self {
            Source::File(path) => path.as_os_str(),
            Source::Bytes(_) => std::ffi::OsStr::new("pipe:0"),
        }
    }

    fn stdin(&self) -> Stdio {
        match self {
            Source::Bytes(_) => Stdio::piped(),
            Source::File(_) => Stdio::null(),
        }
    }
}

/// Ask ffprobe what the video is, so it can be decoded at its own size.
fn probe(source: &Source<'_>) -> Option<([u32; 2], f64, f64)> {
    let mut child = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1",
        ])
        .arg(source.input())
        .stdin(source.stdin())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let (Source::Bytes(bytes), Some(mut stdin)) = (source, child.stdin.take()) {
        let _ = stdin.write_all(bytes);
    }
    let output = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    let width: u32 = field("width")?.parse().ok()?;
    let height: u32 = field("height")?.parse().ok()?;
    let rate = field("r_frame_rate").and_then(|rate| {
        let (top, bottom) = rate.split_once('/')?;
        let (top, bottom): (f64, f64) = (top.parse().ok()?, bottom.parse().ok()?);
        (bottom > 0.0).then_some(top / bottom)
    });
    let duration = field("duration").and_then(|d| d.parse::<f64>().ok());
    Some((
        [width, height],
        rate.unwrap_or(30.0),
        duration.unwrap_or_default(),
    ))
}

/// Decode `bytes` as ffmpeg sees them.
/// What the video at `path` is, without decoding a frame of it.
pub fn details(path: &Path, how: Decode) -> Result<crate::Details, String> {
    let (natural, rate, duration) =
        probe(&Source::File(path)).ok_or("cannot tell what the video is; ffprobe found nothing")?;
    let [width, height] = fit(Some(natural), how.size)?;
    Ok(crate::Details {
        width,
        height,
        rate: how.rate.unwrap_or(rate),
        duration,
    })
}

/// The size to decode at: the video's own, bounded by what was asked for,
/// keeping its shape and never growing it.
fn fit(natural: Option<[u32; 2]>, wanted: Option<[u32; 2]>) -> Result<[u32; 2], String> {
    Ok(match (natural, wanted) {
        (Some([w, h]), Some([max_w, max_h])) => {
            let fit = (f64::from(max_w) / f64::from(w))
                .min(f64::from(max_h) / f64::from(h))
                .min(1.0);
            // Even dimensions: most encoders and pixel formats want them.
            let even = |n: f64| ((n.round() as u32).max(2) / 2) * 2;
            [even(f64::from(w) * fit), even(f64::from(h) * fit)]
        }
        (natural, wanted) => wanted
            .or(natural)
            .ok_or("cannot tell how big the video is; ffprobe found nothing")?,
    })
}

pub fn decode(source: Source<'_>, how: Decode) -> Result<Video, String> {
    let probed = probe(&source);
    let [width, height] = fit(probed.map(|(size, ..)| size), how.size)?;
    let rate = how.rate.or(probed.map(|(_, rate, _)| rate)).unwrap_or(30.0);
    if width == 0 || height == 0 || rate <= 0.0 || rate.is_nan() {
        return Err(format!("{width}x{height} at {rate} fps is not a video"));
    }

    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(source.input())
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-s"])
        .arg(format!("{width}x{height}"))
        .args(["-r", &rate.to_string(), "pipe:1"])
        .stdin(source.stdin())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg did not run ({e}); is it installed?"))?;
    // Feeding and reading at once: a clip is bigger than a pipe buffer, so
    // writing it all before reading would deadlock.
    let feeder = match (&source, child.stdin.take()) {
        (Source::Bytes(bytes), Some(mut stdin)) => {
            let fed: Vec<u8> = bytes.to_vec();
            Some(std::thread::spawn(move || {
                let _ = stdin.write_all(&fed);
            }))
        }
        _ => None,
    };
    let output = child
        .wait_with_output()
        .map_err(|e| format!("ffmpeg failed: {e}"))?;
    if let Some(feeder) = feeder {
        let _ = feeder.join();
    }
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg failed: {}", why.trim()));
    }

    let size = width as usize * height as usize * 4;
    let frames: Vec<Vec<u8>> = output
        .stdout
        .chunks_exact(size)
        .map(<[u8]>::to_vec)
        .collect();
    if frames.is_empty() {
        let why = String::from_utf8_lossy(&output.stderr);
        let why = why.trim();
        return Err(match source {
            // MP4 and friends keep their index at the end of the file.
            Source::Bytes(_) => format!(
                "no frames came out of ffmpeg: this container may need a file to seek in rather than a stream. {why}"
            ),
            Source::File(_) => format!("no frames came out of ffmpeg. {why}"),
        });
    }
    log::debug!(
        "decoded {} frames of {width}x{height} at {rate} fps",
        frames.len()
    );
    Video::from_frames([width, height], rate, frames)
}

/// One frame of the video at `path`, the one showing `position` seconds
/// in, at the size and rate `details` says the clip is read at.
///
/// Seeks there and decodes a single picture, so a still of a clip costs
/// about what a probe costs rather than the whole video. The position is
/// quantised to the clip's own frames first, the way every other reader
/// here picks one, so the same second always gives the same picture.
pub fn still(path: &Path, position: f64, details: crate::Details) -> Result<Vec<u8>, String> {
    let (width, height) = (details.width, details.height);
    let rate = if details.rate > 0.0 {
        details.rate
    } else {
        30.0
    };
    let at = (position.max(0.0) * rate).floor() / rate;
    // Seeking before the input, which is the fast one: ffmpeg jumps to the
    // keyframe before `at` and decodes from there to it, rather than
    // reading the clip from the top.
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &format!("{at}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-s"])
        .arg(format!("{width}x{height}"))
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("ffmpeg did not run ({e}); is it installed?"))?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg failed: {}", why.trim()));
    }
    let wanted = width as usize * height as usize * 4;
    if output.stdout.len() < wanted {
        // Past the end of the clip, or a container that gave nothing.
        let why = String::from_utf8_lossy(&output.stderr);
        return Err(format!("no frame at {at:.3}s. {}", why.trim()));
    }
    let mut frame = output.stdout;
    frame.truncate(wanted);
    Ok(frame)
}

/// Whether the file has an audio stream at all, so a clip without one
/// costs a probe rather than a decode that produces nothing.
pub fn has_sound(path: &Path) -> bool {
    Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "audio")
}

/// Decode the file's audio stream to interleaved stereo f32 at `rate`.
///
/// Raw samples rather than a container: there is nothing to parse on this
/// side, and a pipe cannot carry a WAV header that knows its own length.
/// `None` when the file has no audio.
pub fn soundtrack(path: &Path, rate: u32) -> Result<Option<Vec<f32>>, String> {
    if !has_sound(path) {
        return Ok(None);
    }
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
            "-ac",
            "2",
            "-ar",
            &rate.to_string(),
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("running ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "decoding the soundtrack: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let samples = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect::<Vec<f32>>();
    Ok((!samples.is_empty()).then_some(samples))
}
