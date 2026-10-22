//! Making derivatives, resizing and stripping metadata, with the real tools.
//!
//! Needs `vips`, `ffmpeg` and `ffprobe` on the PATH, as CI and the Nix shell
//! provide. Every test works on a copy in a temporary directory: derivatives
//! are written beside their source.

use haloid_core::media::{Derivative, MediaKind};
use haloid_core::processor::{DerivativePlan, MediaProcessor};
use haloid_core::settings::{MediumDimension, Settings};
use haloid_media::{MediaConfig, MediaToolkit};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/media")
        .join(name)
}

fn toolkit() -> MediaToolkit {
    MediaToolkit::new(MediaConfig::default())
}

/// Copy a fixture into `dir` under a name without an extension, the way the
/// pipeline's temp files arrive.
fn staged(dir: &TempDir, name: &str) -> PathBuf {
    let to = dir
        .path()
        .join(format!("upload-{}", name.replace('.', "-")));
    std::fs::copy(fixture(name), &to).unwrap();
    to
}

/// A larger image than any fixture, made on the spot so it is not committed.
fn generated_jpeg(dir: &TempDir, width: u32, height: u32) -> PathBuf {
    let path = dir.path().join(format!("upload-{width}x{height}"));
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("testsrc=size={width}x{height}"))
        .args(["-frames:v", "1", "-f", "mjpeg"])
        .arg(&path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success());
    path
}

fn plan(kind: MediaKind) -> DerivativePlan {
    DerivativePlan::new(kind, &Settings::default().derivatives)
}

#[tokio::test]
async fn a_large_photo_gets_a_thumb_and_a_medium() {
    let dir = TempDir::new().unwrap();
    let source = generated_jpeg(&dir, 800, 600);
    let toolkit = toolkit();
    let probe = toolkit.probe(&source).await.unwrap();

    let set = toolkit
        .derive(&source, &probe, &plan(MediaKind::Image))
        .await
        .unwrap();
    assert_eq!(set.chain().bits(), 7);
    assert_eq!(
        set.chain(),
        plan(MediaKind::Image).expected_chain(&probe, false)
    );

    let thumb = set.get(Derivative::Thumb).unwrap();
    assert_eq!(
        (thumb.width, thumb.height),
        (160, 160),
        "centre-cropped square"
    );
    assert_eq!(
        (thumb.mime.as_str(), thumb.extension.as_str()),
        ("image/jpeg", "jpg")
    );
    assert!(thumb.path.exists() && thumb.size > 0);
    assert!(thumb.path.to_string_lossy().ends_with(".th.jpg"));

    let medium = set.get(Derivative::Medium).unwrap();
    assert_eq!(
        (medium.width, medium.height),
        (500, 375),
        "500 wide, in proportion"
    );

    let image = set.get(Derivative::Image).unwrap();
    assert_eq!(image.path, source, "the served file is the upload itself");
}

#[tokio::test]
async fn a_small_photo_gets_no_medium_but_a_full_size_thumb() {
    // Thumbs are forced to their square even from a smaller source, as
    // upstream does; mediums never enlarge, so they are skipped instead.
    let dir = TempDir::new().unwrap();
    let source = staged(&dir, "still.jpg");
    let toolkit = toolkit();
    let probe = toolkit.probe(&source).await.unwrap();
    let set = toolkit
        .derive(&source, &probe, &plan(MediaKind::Image))
        .await
        .unwrap();

    assert_eq!(set.chain().bits(), 5);
    let thumb = set.get(Derivative::Thumb).unwrap();
    assert_eq!((thumb.width, thumb.height), (160, 160));
}

#[tokio::test]
async fn the_medium_follows_the_configured_side() {
    let dir = TempDir::new().unwrap();
    let source = generated_jpeg(&dir, 400, 1200);
    let toolkit = toolkit();
    let probe = toolkit.probe(&source).await.unwrap();

    let by_width = toolkit
        .derive(&source, &probe, &plan(MediaKind::Image))
        .await
        .unwrap();
    assert!(
        by_width.get(Derivative::Medium).is_none(),
        "400 wide is under the width threshold"
    );

    let mut settings = Settings::default().derivatives;
    settings.medium_fixed_dimension = MediumDimension::Longest;
    let by_longest = toolkit
        .derive(
            &source,
            &probe,
            &DerivativePlan::new(MediaKind::Image, &settings),
        )
        .await
        .unwrap();
    let medium = by_longest.get(Derivative::Medium).unwrap();
    assert_eq!((medium.width, medium.height), (167, 500));
}

#[tokio::test]
async fn derivatives_of_an_animation_are_still_unless_asked() {
    let dir = TempDir::new().unwrap();
    let source = staged(&dir, "anim.gif");
    let toolkit = toolkit();
    let probe = toolkit.probe(&source).await.unwrap();

    let set = toolkit
        .derive(&source, &probe, &plan(MediaKind::Image))
        .await
        .unwrap();
    let thumb = set.get(Derivative::Thumb).unwrap();
    let thumb_probe = toolkit.probe(&thumb.path).await.unwrap();
    assert!(!thumb_probe.is_animated, "a static first frame by default");

    let mut settings = Settings::default().derivatives;
    settings.animated_derivatives = true;
    let animated_dir = TempDir::new().unwrap();
    let source = staged(&animated_dir, "anim.gif");
    let set = toolkit
        .derive(
            &source,
            &probe,
            &DerivativePlan::new(MediaKind::Image, &settings),
        )
        .await
        .unwrap();
    let thumb_probe = toolkit
        .probe(&set.get(Derivative::Thumb).unwrap().path)
        .await
        .unwrap();
    assert_eq!(
        thumb_probe.frame_count,
        Some(3),
        "animation kept when asked"
    );
}

#[tokio::test]
async fn heic_derivatives_are_jpeg_so_every_browser_can_show_them() {
    let dir = TempDir::new().unwrap();
    let source = staged(&dir, "still.heic");
    let toolkit = toolkit();
    let probe = toolkit.probe(&source).await.unwrap();
    let set = toolkit
        .derive(&source, &probe, &plan(MediaKind::Image))
        .await
        .unwrap();
    let thumb = set.get(Derivative::Thumb).unwrap();
    assert_eq!(thumb.extension, "jpg");
    assert_eq!(
        toolkit.probe(&thumb.path).await.unwrap().mime,
        "image/jpeg",
        "the bytes really are JPEG"
    );
}

/// Stripping HEIC and AVIF re-encodes them, which needs libheif's
/// *encoder* plugins (x265, aomenc) as well as its decoders. A host with
/// only the decoders accepts the upload and then fails it: this is the
/// test that says so.
#[tokio::test]
async fn heic_and_avif_can_be_stripped_which_needs_the_encoders() {
    let toolkit = toolkit();
    for name in ["still.heic", "still.avif"] {
        let dir = TempDir::new().unwrap();
        let source = staged(&dir, name);
        let probe = toolkit.probe(&source).await.unwrap();
        toolkit
            .strip_metadata(&source, &probe)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "{name}: {e:?} — are libheif-plugin-x265 and libheif-plugin-aomenc installed?"
                )
            });
        let after = toolkit.probe(&source).await.unwrap();
        assert_eq!(
            (after.width, after.height),
            (probe.width, probe.height),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_video_gets_a_frame_and_derivatives_from_it() {
    let dir = TempDir::new().unwrap();
    let toolkit = toolkit();

    let source = staged(&dir, "clip.mp4");
    let probe = toolkit.probe(&source).await.unwrap();
    let set = toolkit
        .derive(&source, &probe, &plan(MediaKind::Video))
        .await
        .unwrap();
    assert_eq!(set.chain().bits(), 21, "upstream's documented video chain");
    let frame = set.get(Derivative::Frame).unwrap();
    assert_eq!((frame.width, frame.height), (32, 16));
    assert_eq!(frame.extension, "jpg");
    assert!(frame.path.to_string_lossy().ends_with(".fr.jpg"));
    assert_eq!(set.get(Derivative::Thumb).unwrap().extension, "jpg");

    // A rotated video's frame comes out the way it is displayed.
    let rotated = staged(&dir, "rotated.mp4");
    let probe = toolkit.probe(&rotated).await.unwrap();
    let set = toolkit
        .derive(&rotated, &probe, &plan(MediaKind::Video))
        .await
        .unwrap();
    let frame = set.get(Derivative::Frame).unwrap();
    assert_eq!((frame.width, frame.height), (16, 32));
}

#[tokio::test]
async fn audio_gets_nothing_invented() {
    let dir = TempDir::new().unwrap();
    let source = staged(&dir, "tone.mp3");
    let toolkit = toolkit();
    let probe = toolkit.probe(&source).await.unwrap();
    let set = toolkit
        .derive(&source, &probe, &plan(MediaKind::Audio))
        .await
        .unwrap();
    assert_eq!(set.chain().bits(), 4);
}

#[tokio::test]
async fn resizing_never_enlarges_and_honours_orientation() {
    let dir = TempDir::new().unwrap();
    let toolkit = toolkit();

    let large = generated_jpeg(&dir, 800, 600);
    let resized = toolkit.resize(&large, 200).await.unwrap();
    assert_ne!(resized, large, "the original is left in place");
    assert!(large.exists());
    let probe = toolkit.probe(&resized).await.unwrap();
    assert_eq!((probe.width, probe.height), (200, 150));

    // Displayed 16x32; 10 wide keeps the displayed proportion, upright.
    let rotated = staged(&dir, "rotated.jpg");
    let probe = toolkit
        .probe(&toolkit.resize(&rotated, 10).await.unwrap())
        .await
        .unwrap();
    assert_eq!((probe.width, probe.height), (10, 20));

    let small = staged(&dir, "still.jpg");
    let probe = toolkit
        .probe(&toolkit.resize(&small, 1000).await.unwrap())
        .await
        .unwrap();
    assert_eq!(probe.width, 32, "never enlarged");
}

#[tokio::test]
async fn animations_are_not_resized() {
    // Upstream refuses too: resizing an animation frame by frame is
    // expensive and usually not what was meant.
    let dir = TempDir::new().unwrap();
    let source = staged(&dir, "anim.gif");
    assert_eq!(toolkit().resize(&source, 10).await.unwrap(), source);
}

#[tokio::test]
async fn exif_is_captured_without_makernote_gps_or_bulk() {
    let toolkit = toolkit();
    let json = toolkit
        .read_exif(&fixture("exif.jpg"))
        .await
        .unwrap()
        .expect("the fixture has EXIF");
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(
        map.get("Make").and_then(|v| v.as_str()),
        Some("\"HaloidCam\"")
    );
    assert!(map.contains_key("Orientation"));
    assert!(
        !map.keys().any(|k| k.starts_with("GPS")),
        "location must never be stored: {map:?}"
    );
    assert!(
        !map.contains_key("ImageDescription"),
        "over 4 KiB is dropped"
    );

    assert_eq!(
        toolkit.read_exif(&fixture("still.png")).await.unwrap(),
        None
    );
    assert_eq!(toolkit.read_exif(&fixture("clip.mp4")).await.unwrap(), None);
}

#[tokio::test]
async fn stripping_a_jpeg_is_lossless_and_keeps_its_orientation() {
    let dir = TempDir::new().unwrap();
    let source = staged(&dir, "exif.jpg");
    let toolkit = toolkit();
    let probe = toolkit.probe(&source).await.unwrap();
    let before = std::fs::read(&source).unwrap();

    toolkit.strip_metadata(&source, &probe).await.unwrap();
    let after = std::fs::read(&source).unwrap();

    for needle in [&b"HaloidCam"[..], b"secret comment", b"AAAAAAAAAAAAAAAA"] {
        assert!(
            !after.windows(needle.len()).any(|w| w == needle),
            "{:?} survived",
            String::from_utf8_lossy(needle)
        );
    }
    let probe_after = toolkit.probe(&source).await.unwrap();
    assert_eq!(
        (probe_after.width, probe_after.height),
        (16, 32),
        "still displayed rotated"
    );
    // Lossless: everything from the start of scan onward is byte-identical.
    let sos = |b: &[u8]| b.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
    assert_eq!(&before[sos(&before)..], &after[sos(&after)..]);
}

#[tokio::test]
async fn stripping_a_png_drops_text_chunks() {
    let dir = TempDir::new().unwrap();
    let source = staged(&dir, "still.png");
    // Splice a tEXt chunk in after IHDR.
    let mut bytes = std::fs::read(&source).unwrap();
    let text = b"Comment\0where I live";
    let mut chunk = (text.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(text);
    chunk.extend_from_slice(&crc32(b"tEXt", text).to_be_bytes());
    bytes.splice(33..33, chunk);
    std::fs::write(&source, &bytes).unwrap();

    let toolkit = toolkit();
    let probe = toolkit.probe(&source).await.unwrap();
    toolkit.strip_metadata(&source, &probe).await.unwrap();
    let after = std::fs::read(&source).unwrap();
    assert!(!after.windows(8).any(|w| w == b"where I "));
    assert!(toolkit.probe(&source).await.is_ok(), "still a valid PNG");
}

#[tokio::test]
async fn stripping_a_video_removes_tags_but_keeps_its_rotation() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("upload-tagged");
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(fixture("rotated.mp4"))
        .args([
            "-c",
            "copy",
            "-metadata",
            "title=my house",
            "-metadata",
            "location=+51.5000-000.1000/",
            "-f",
            "mp4",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());

    let toolkit = toolkit();
    let probe = toolkit.probe(&source).await.unwrap();
    toolkit.strip_metadata(&source, &probe).await.unwrap();

    let after = std::fs::read(&source).unwrap();
    assert!(!after.windows(8).any(|w| w == b"my house"));
    assert!(!after.windows(8).any(|w| w == b"+51.5000"));
    let probe = toolkit.probe(&source).await.unwrap();
    assert_eq!((probe.width, probe.height), (16, 32), "rotation survived");
}

fn crc32(kind: &[u8], data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in kind.iter().chain(data) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
