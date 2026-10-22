//! Probing real files: the committed fixtures in `fixtures/media/`, which
//! `scripts/make-media-fixtures.sh` regenerates.
//!
//! Video and audio need `ffprobe` on the PATH, as CI and the Nix shell
//! provide. These tests fail rather than skip without it: from this chunk on,
//! the media tools are a requirement, not an option.

use haloid_core::MediaError;
use haloid_core::media::MediaKind;
use haloid_core::processor::MediaProcessor;
use haloid_media::{MediaConfig, MediaToolkit};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/media")
        .join(name)
}

fn toolkit() -> MediaToolkit {
    MediaToolkit::new(MediaConfig::default())
}

struct Expect {
    file: &'static str,
    kind: MediaKind,
    mime: &'static str,
    extension: &'static str,
    width: u32,
    height: u32,
    animated: bool,
    frames: Option<u32>,
    has_duration: bool,
}

const fn still(file: &'static str, mime: &'static str, extension: &'static str) -> Expect {
    Expect {
        file,
        kind: MediaKind::Image,
        mime,
        extension,
        width: 32,
        height: 16,
        animated: false,
        frames: None,
        has_duration: false,
    }
}

const fn animated(file: &'static str, mime: &'static str, extension: &'static str) -> Expect {
    Expect {
        animated: true,
        frames: Some(3),
        ..still(file, mime, extension)
    }
}

const fn timed(
    file: &'static str,
    kind: MediaKind,
    mime: &'static str,
    extension: &'static str,
    width: u32,
    height: u32,
) -> Expect {
    Expect {
        file,
        kind,
        mime,
        extension,
        width,
        height,
        animated: false,
        frames: None,
        has_duration: true,
    }
}

#[tokio::test]
async fn every_accepted_format_probes_correctly() {
    let cases = [
        still("still.jpg", "image/jpeg", "jpg"),
        // Stored 32x16 with EXIF orientation 6: displayed, and recorded,
        // as 16x32.
        Expect {
            width: 16,
            height: 32,
            ..still("rotated.jpg", "image/jpeg", "jpg")
        },
        still("still.png", "image/png", "png"),
        still("still.gif", "image/gif", "gif"),
        still("still.webp", "image/webp", "webp"),
        still("still.avif", "image/avif", "avif"),
        still("still.heic", "image/heic", "heic"),
        animated("anim.gif", "image/gif", "gif"),
        animated("anim.png", "image/apng", "png"),
        animated("anim.webp", "image/webp", "webp"),
        timed("clip.mp4", MediaKind::Video, "video/mp4", "mp4", 32, 16),
        // A 90° display matrix: the frames are 32x16, the video is 16x32.
        timed("rotated.mp4", MediaKind::Video, "video/mp4", "mp4", 16, 32),
        timed("clip.webm", MediaKind::Video, "video/webm", "webm", 32, 16),
        timed("tone.mp3", MediaKind::Audio, "audio/mpeg", "mp3", 0, 0),
        timed("tone.wav", MediaKind::Audio, "audio/wav", "wav", 0, 0),
        timed("tone.m4a", MediaKind::Audio, "audio/mp4", "m4a", 0, 0),
    ];

    let toolkit = toolkit();
    for case in cases {
        let probe = toolkit
            .probe(&fixture(case.file))
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", case.file));
        let file = case.file;
        assert_eq!(probe.kind, case.kind, "{file} kind");
        assert_eq!(probe.mime, case.mime, "{file} mime");
        assert_eq!(probe.extension, case.extension, "{file} extension");
        assert_eq!(
            (probe.width, probe.height),
            (case.width, case.height),
            "{file} dimensions"
        );
        assert_eq!(probe.is_animated, case.animated, "{file} animation");
        assert_eq!(probe.frame_count, case.frames, "{file} frames");
        match (case.has_duration, probe.duration_ms) {
            (true, Some(ms)) => assert!((900..=1200).contains(&ms), "{file} lasts {ms} ms"),
            (false, None) => {}
            (_, other) => panic!("{file}: unexpected duration {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_decompression_bomb_is_measured_from_its_header_alone() {
    // 100000x100000 claimed over 69 bytes. Probing reports the claim, fast
    // and without allocating it, so the caller's pixel limit refuses it
    // before anything decodes.
    let started = Instant::now();
    let probe = toolkit().probe(&fixture("bomb.png")).await.unwrap();
    assert_eq!(probe.pixels(), 10_000_000_000);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "probing took {:?}: something decoded",
        started.elapsed()
    );
}

#[tokio::test]
async fn svg_is_refused_whatever_it_is_called() {
    for name in ["xss.svg", "svg-named.jpg"] {
        match toolkit().probe(&fixture(name)).await {
            Err(MediaError::Unsupported(reason)) => {
                assert!(reason.to_lowercase().contains("svg"), "{name}: {reason}")
            }
            other => panic!("{name}: expected Unsupported, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn unrecognised_bytes_are_unsupported() {
    assert!(matches!(
        toolkit().probe(&fixture("not-media.bin")).await,
        Err(MediaError::Unsupported(_))
    ));
}

#[tokio::test]
async fn a_truncated_video_is_a_probe_failure() {
    // The legacy API reports this as code 600, so it must be ProbeFailed,
    // not Unsupported: the type was recognised, the file was broken.
    let dir = tempfile::TempDir::new().unwrap();
    let truncated = dir.path().join("broken.mp4");
    let bytes = std::fs::read(fixture("clip.mp4")).unwrap();
    std::fs::write(&truncated, &bytes[..64]).unwrap();

    match toolkit().probe(&truncated).await {
        Err(MediaError::ProbeFailed(_)) => {}
        other => panic!("expected ProbeFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn a_missing_ffprobe_is_reported_as_unavailable() {
    let toolkit = MediaToolkit::new(MediaConfig {
        ffprobe: "/nonexistent/ffprobe".into(),
        ..MediaConfig::default()
    });
    assert!(matches!(
        toolkit.probe(&fixture("clip.mp4")).await,
        Err(MediaError::ToolUnavailable("ffprobe"))
    ));
    // Images never needed it.
    assert!(toolkit.probe(&fixture("still.png")).await.is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn a_hung_ffprobe_is_killed_at_the_timeout() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().unwrap();
    let fake = dir.path().join("ffprobe");
    std::fs::write(&fake, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let toolkit = MediaToolkit::new(MediaConfig {
        ffprobe: fake,
        timeout: Duration::from_millis(300),
        ..MediaConfig::default()
    });
    let started = Instant::now();
    match toolkit.probe(&fixture("clip.mp4")).await {
        Err(MediaError::Timeout { .. }) => {}
        other => panic!("expected Timeout, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the timeout did not fire"
    );
}

#[tokio::test]
async fn truncated_images_never_panic_the_probe() {
    // Uploads arrive cut short all the time. Every prefix of every image
    // fixture must come back as Ok or Err — never a panic in a header
    // parser, which would take the request's task down with it.
    let dir = tempfile::TempDir::new().unwrap();
    let toolkit = toolkit();
    for name in [
        "still.jpg",
        "rotated.jpg",
        "still.png",
        "anim.png",
        "still.gif",
        "anim.gif",
        "still.webp",
        "anim.webp",
        "still.avif",
        "still.heic",
        "bomb.png",
    ] {
        let bytes = std::fs::read(fixture(name)).unwrap();
        let path = dir.path().join(name);
        for len in (0..bytes.len()).step_by(7) {
            std::fs::write(&path, &bytes[..len]).unwrap();
            let _ = toolkit.probe(&path).await;
        }
    }
}
