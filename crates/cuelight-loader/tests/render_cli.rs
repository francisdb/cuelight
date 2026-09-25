//! The `cuelight-render` tool end to end, on a show with a video.
//!
//! Needs ffmpeg to make and decode the clip, and a GPU adapter to draw
//! the frames; without either the test says so and passes, the way the
//! renderer's own tests do.

#![cfg(feature = "render-cli")]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Two seconds of clip: a red second, then a blue one, at 30 fps.
///
/// Colours come back a shade off what went in, since the clip is encoded
/// as YUV and decoded back, so they are read with a tolerance.
fn clip(into: &Path) -> bool {
    let made = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=64x64:d=1:r=30",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=64x64:d=1:r=30",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1[v]",
            "-map",
            "[v]",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(into)
        .status();
    match made {
        Ok(status) if status.success() => true,
        _ => {
            eprintln!("no ffmpeg: skipping the video render test");
            false
        }
    }
}

/// A show folder whose only layer is a round window over the clip. The
/// window slides to the right and the picture slides back by the same
/// amount, so what shows is the middle of the picture through a circle
/// that has moved: a frame drawn in the wrong place, or without the
/// group's clip, lands somewhere else entirely.
fn show(tag: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("cuelight-render-{tag}-{}", std::process::id()));
    let videos = dir.join("assets/videos");
    std::fs::create_dir_all(&videos).unwrap();
    if !clip(&videos.join("clip.mp4")) {
        return None;
    }
    std::fs::write(
        dir.join("show.json"),
        r##"{ "name": "masked", "size": [128, 64], "background": "#000000",
             "variables": { "slide": 0 },
             "layers": [
               { "name": "window", "type": "group", "clip": { "circle": [32, 32, 16] },
                 "bindings": [{ "property": "x", "variable": "slide" }],
                 "children": [
                   { "name": "picture", "type": "video", "video": "clip", "autoplay": true,
                     "size": [64, 64],
                     "bindings": [{ "property": "x", "variable": "slide", "scale": -1 }] }
                 ] }
             ] }"##,
    )
    .unwrap();
    Some(dir)
}

/// One pixel of a written frame, as RGBA.
fn pixel(file: &Path, x: u32, y: u32) -> [u8; 4] {
    let file = std::io::BufReader::new(std::fs::File::open(file).unwrap());
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let i = ((y * info.width + x) * 4) as usize;
    buf[i..i + 4].try_into().unwrap()
}

#[test]
fn a_video_layer_draws_the_frame_it_is_playing() {
    let Some(dir) = show("video") else { return };
    let out = dir.join("frames");
    // Halfway through each second of the clip, with the window and the
    // picture slid sixteen across, so the circle sits inside the picture.
    let run = Command::new(env!("CARGO_BIN_EXE_cuelight-render"))
        .arg(&dir)
        .args(["--at", "0.5,1.5", "--set", "0:slide=16", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&run.stderr).into_owned();
    if said.contains("no renderer") {
        eprintln!("no GPU adapter: skipping the video render test");
        return;
    }
    assert!(run.status.success(), "{said}");
    assert!(!said.contains("warning: video"), "{said}");

    let near = |got: [u8; 4], want: [u8; 3]| {
        let close = (0..3).all(|i| got[i].abs_diff(want[i]) < 8);
        assert!(close, "{got:?} is not {want:?}");
    };
    // The clip's own colours, a second apart: the frame written at a
    // time is the frame the engine says is playing then.
    near(pixel(&out.join("t0000.500.png"), 48, 32), [255, 0, 0]);
    near(pixel(&out.join("t0001.500.png"), 48, 32), [0, 0, 255]);
    // And only through the window: the picture covers the top of the
    // canvas too, and the group's clip cuts it away.
    near(pixel(&out.join("t0000.500.png"), 48, 4), [0, 0, 0]);
    near(pixel(&out.join("t0000.500.png"), 100, 32), [0, 0, 0]);
    std::fs::remove_dir_all(&dir).ok();
}
