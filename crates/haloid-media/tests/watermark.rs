//! Watermarks, checked the way a person would: the mark's corner got
//! brighter, the rest of the picture did not change, and the whole still
//! looks like the same picture by perceptual hash. Never byte equality —
//! a libvips upgrade re-encodes differently and must not fail the suite.

use haloid_core::processor::{MediaProcessor, Watermark, WatermarkPosition};
use haloid_media::{MediaConfig, MediaToolkit};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/media")
        .join(name)
}

fn vips(args: &[&str]) -> String {
    let out = Command::new("vips")
        .args(args)
        .env("VIPS_WARNING", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "vips {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A dark, textured photograph-like image, big enough to take a mark.
fn photo(dir: &TempDir, width: u32, height: u32) -> PathBuf {
    let noise = dir.path().join("noise.v");
    vips(&[
        "gaussnoise",
        noise.to_str().unwrap(),
        &width.to_string(),
        &height.to_string(),
        "--mean",
        "60",
        "--sigma",
        "20",
    ]);
    let colour = dir.path().join("colour.v");
    vips(&[
        "bandjoin",
        &format!("{0} {0} {0}", noise.display()),
        colour.to_str().unwrap(),
    ]);
    let cast = dir.path().join("cast.v");
    vips(&[
        "cast",
        colour.to_str().unwrap(),
        cast.to_str().unwrap(),
        "uchar",
    ]);
    let path = dir.path().join("upload-photo");
    vips(&[
        "copy",
        cast.to_str().unwrap(),
        &format!("{}.jpg", path.display()),
    ]);
    std::fs::rename(format!("{}.jpg", path.display()), &path).unwrap();
    path
}

/// A solid white mark with no alpha channel, so the code must add one.
fn white_mark(dir: &TempDir) -> PathBuf {
    let black = dir.path().join("black.v");
    vips(&[
        "black",
        black.to_str().unwrap(),
        "200",
        "100",
        "--bands",
        "3",
    ]);
    let mark = dir.path().join("mark.png");
    vips(&[
        "linear",
        black.to_str().unwrap(),
        mark.to_str().unwrap(),
        "1",
        "255",
        "--uchar",
    ]);
    mark
}

fn mark(path: PathBuf, position: WatermarkPosition, opacity: u8) -> Watermark {
    Watermark {
        mark: path,
        position,
        width_percent: 25,
        opacity_percent: opacity,
        margin: 10,
        min_width: 100,
        min_height: 100,
    }
}

/// Mean brightness of a rectangle.
fn brightness(path: &Path, x: u32, y: u32, w: u32, h: u32) -> f64 {
    let region = path.with_extension("region.v");
    vips(&[
        "crop",
        path.to_str().unwrap(),
        region.to_str().unwrap(),
        &x.to_string(),
        &y.to_string(),
        &w.to_string(),
        &h.to_string(),
    ]);
    vips(&["avg", region.to_str().unwrap()]).parse().unwrap()
}

/// A 64-bit difference hash of the whole picture.
fn dhash(path: &Path) -> u64 {
    let small = path.with_extension("small.v");
    vips(&[
        "thumbnail",
        path.to_str().unwrap(),
        small.to_str().unwrap(),
        "9",
        "--height",
        "8",
        "--size",
        "force",
    ]);
    let grey = path.with_extension("grey.v");
    vips(&[
        "colourspace",
        small.to_str().unwrap(),
        grey.to_str().unwrap(),
        "b-w",
    ]);
    let raw = path.with_extension("raw");
    vips(&["rawsave", grey.to_str().unwrap(), raw.to_str().unwrap()]);
    let pixels = std::fs::read(&raw).unwrap();
    assert_eq!(pixels.len(), 72);
    let mut hash = 0u64;
    for row in 0..8 {
        for col in 0..8 {
            hash <<= 1;
            if pixels[row * 9 + col] < pixels[row * 9 + col + 1] {
                hash |= 1;
            }
        }
    }
    hash
}

#[tokio::test]
async fn the_mark_lands_in_its_corner_and_nowhere_else() {
    let dir = TempDir::new().unwrap();
    let source = photo(&dir, 800, 600);
    let toolkit = MediaToolkit::new(MediaConfig::default());
    let out = toolkit
        .watermark(
            &source,
            &mark(white_mark(&dir), WatermarkPosition::BottomRight, 80),
        )
        .await
        .unwrap();
    assert_ne!(out, source, "a new file");
    assert!(source.exists(), "the original is kept");

    // 25% of 800 is 200 wide; the 200x100 mark stays 2:1, so 200x100, at
    // 10 px from the bottom-right corner: x 590..790, y 490..590.
    let before = brightness(&source, 600, 500, 180, 80);
    let after = brightness(&out, 600, 500, 180, 80);
    assert!(
        after > before + 100.0,
        "the corner is marked: {before} -> {after}"
    );
    let untouched_before = brightness(&source, 10, 10, 300, 200);
    let untouched_after = brightness(&out, 10, 10, 300, 200);
    assert!(
        (untouched_before - untouched_after).abs() < 3.0,
        "{untouched_before} vs {untouched_after}"
    );

    let distance = (dhash(&source) ^ dhash(&out)).count_ones();
    assert!(
        distance <= 16,
        "still the same picture, perceptually: {distance} bits differ"
    );
}

#[tokio::test]
async fn opacity_fades_the_mark() {
    let dir = TempDir::new().unwrap();
    let source = photo(&dir, 800, 600);
    let toolkit = MediaToolkit::new(MediaConfig::default());
    let mark_path = white_mark(&dir);
    let strong = toolkit
        .watermark(
            &source,
            &mark(mark_path.clone(), WatermarkPosition::TopLeft, 100),
        )
        .await
        .unwrap();
    let strong_level = brightness(&strong, 20, 20, 180, 80);
    let faint = toolkit
        .watermark(&source, &mark(mark_path, WatermarkPosition::TopLeft, 30))
        .await
        .unwrap();
    let faint_level = brightness(&faint, 20, 20, 180, 80);
    let base = brightness(&source, 20, 20, 180, 80);
    assert!(strong_level > 240.0, "{strong_level}");
    assert!(
        faint_level > base + 30.0 && faint_level < strong_level - 60.0,
        "{base} < {faint_level} < {strong_level}"
    );
}

#[tokio::test]
async fn small_files_audio_and_animated_png_are_left_alone() {
    let dir = TempDir::new().unwrap();
    let toolkit = MediaToolkit::new(MediaConfig::default());
    // The fixtures are 32 by 16: under the minimum, every one is skipped.
    let watermark = mark(white_mark(&dir), WatermarkPosition::Center, 80);
    for name in ["still.png", "anim.gif", "clip.mp4", "tone.mp3"] {
        let staged = dir
            .path()
            .join(format!("upload-{}", name.replace('.', "-")));
        std::fs::copy(fixture(name), &staged).unwrap();
        let out = toolkit.watermark(&staged, &watermark).await.unwrap();
        assert_eq!(out, staged, "{name} is untouched");
    }
    // Large enough, but vips reads only the first frame of an animated PNG,
    // and marking that would throw the animation away.
    let mut any_size = watermark.clone();
    any_size.min_width = 1;
    any_size.min_height = 1;
    for name in ["anim.png", "tone.mp3"] {
        let staged = dir.path().join(format!("any-{}", name.replace('.', "-")));
        std::fs::copy(fixture(name), &staged).unwrap();
        assert_eq!(
            toolkit.watermark(&staged, &any_size).await.unwrap(),
            staged,
            "{name}"
        );
    }
}

fn ffmpeg(args: &[&str]) {
    let status = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg {args:?}");
}

/// The video and audio streams of a file, by codec.
fn streams(path: &Path) -> Vec<String> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn every_frame_of_an_animation_is_marked_and_the_animation_kept() {
    let dir = TempDir::new().unwrap();
    let toolkit = MediaToolkit::new(MediaConfig::default());
    let watermark = mark(white_mark(&dir), WatermarkPosition::BottomRight, 100);
    for extension in ["gif", "webp"] {
        // Three black frames, 200 by 100, with a grey box that moves along
        // the top so no two frames are alike (an encoder merges those).
        let source = dir.path().join(format!("frames.{extension}"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "color=black:s=200x100:r=10:d=0.3,format=yuv444p,\
             geq=lum='if(between(X\\,100+N*20\\,119+N*20)*lt(Y\\,20)\\,128\\,0)':cb=128:cr=128",
            "-loop",
            "0",
            source.to_str().unwrap(),
        ]);
        let staged = dir.path().join(format!("upload-anim-{extension}"));
        std::fs::copy(&source, &staged).unwrap();
        let probe = toolkit.probe(&staged).await.unwrap();
        assert!(probe.is_animated, "{extension} fixture is animated");

        let out = toolkit.watermark(&staged, &watermark).await.unwrap();
        assert_ne!(out, staged);
        let marked = toolkit.probe(&out).await.unwrap();
        assert!(marked.is_animated, "{extension} is still animated");
        assert_eq!((marked.width, marked.height), (200, 100));
        for page in 0..3 {
            // Flattened: animation frames come with alpha, which would count
            // in the average.
            let frame = dir.path().join(format!("{extension}-frame{page}.png"));
            vips(&[
                "flatten",
                &format!("{}[page={page}]", out.display()),
                frame.to_str().unwrap(),
            ]);
            // The mark is 50 by 25, 10 in from the bottom right corner.
            assert!(
                brightness(&frame, 145, 70, 40, 15) > 200.0,
                "{extension} frame {page} is marked"
            );
            assert!(
                brightness(&frame, 10, 10, 40, 40) < 30.0,
                "{extension} frame {page} is otherwise untouched"
            );
        }
    }
}

#[tokio::test]
async fn a_video_is_marked_on_every_frame_and_keeps_its_sound() {
    let dir = TempDir::new().unwrap();
    let toolkit = MediaToolkit::new(MediaConfig::default());
    let watermark = mark(white_mark(&dir), WatermarkPosition::BottomRight, 100);
    for (extension, codecs) in [
        ("mp4", ["-c:v", "libx264", "-c:a", "aac"]),
        ("webm", ["-c:v", "libvpx-vp9", "-c:a", "libopus"]),
    ] {
        let source = dir.path().join(format!("clip.{extension}"));
        let mut args = vec![
            "-f",
            "lavfi",
            "-i",
            "color=black:s=200x100:r=10:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-shortest",
        ];
        args.extend(codecs);
        args.push(source.to_str().unwrap());
        ffmpeg(&args);
        let staged = dir.path().join(format!("upload-clip-{extension}"));
        std::fs::copy(&source, &staged).unwrap();

        let out = toolkit.watermark(&staged, &watermark).await.unwrap();
        assert_ne!(out, staged);
        assert!(
            out.to_string_lossy().ends_with(extension),
            "{}",
            out.display()
        );
        assert_eq!(
            streams(&out),
            ["video", "audio"],
            "{extension} keeps its sound"
        );
        let probe = toolkit.probe(&out).await.unwrap();
        assert_eq!((probe.width, probe.height), (200, 100));
        assert!(
            probe.duration_ms.unwrap() >= 1800,
            "{extension} is as long as it was"
        );
        // A frame from the middle: marked in its corner, black elsewhere.
        let frame = dir.path().join(format!("{extension}-middle.png"));
        ffmpeg(&[
            "-ss",
            "1",
            "-i",
            out.to_str().unwrap(),
            "-frames:v",
            "1",
            frame.to_str().unwrap(),
        ]);
        assert!(
            brightness(&frame, 145, 70, 40, 15) > 200.0,
            "{extension} is marked"
        );
        assert!(
            brightness(&frame, 10, 10, 40, 40) < 30.0,
            "{extension} is otherwise untouched"
        );
    }
}

#[tokio::test]
async fn a_rotated_photo_is_marked_the_way_it_is_seen() {
    let dir = TempDir::new().unwrap();
    let toolkit = MediaToolkit::new(MediaConfig::default());
    let staged = dir.path().join("upload-rotated");
    std::fs::copy(fixture("rotated.jpg"), &staged).unwrap();
    let probe = toolkit.probe(&staged).await.unwrap();
    let mut watermark = mark(white_mark(&dir), WatermarkPosition::BottomRight, 80);
    watermark.min_width = 1;
    watermark.min_height = 1;
    let out = toolkit.watermark(&staged, &watermark).await.unwrap();
    let marked = toolkit.probe(&out).await.unwrap();
    assert_eq!(
        (marked.width, marked.height),
        (probe.width, probe.height),
        "upright, as displayed"
    );
}
