//! Making thumbs, mediums and video frames, and resizing the served file.
//!
//! All pixel work is `vips thumbnail` in a subprocess, and frame extraction
//! is `ffmpeg`; see [`crate::tool`]. `thumbnail` autorotates, so every
//! derivative comes out the way the original is displayed, and it decodes
//! with shrink-on-load, so a large JPEG is never fully decoded to make a
//! 160px thumb.
//!
//! Derivatives are written beside their source as `{stem}.th.{ext}`,
//! `{stem}.md.{ext}` and `{stem}.fr.jpg`.

use crate::MediaConfig;
use crate::tool::Tool;
use haloid_core::MediaError;
use haloid_core::media::{Derivative, MediaKind, ProbeResult};
use haloid_core::processor::{DerivativeFile, DerivativePlan, DerivativeSet};
use haloid_core::settings::MediumDimension;
use std::path::{Path, PathBuf};

/// Larger than any real image: "no constraint" on one side of a resize.
pub(crate) const UNBOUNDED: u32 = 10_000_000;

pub(crate) fn failed(reason: String) -> MediaError {
    MediaError::DeriveFailed(reason)
}

/// vips's save options for a format: modest quality, and only the colour
/// profile kept — no EXIF, no GPS, no orientation (the pixels are already
/// upright).
pub(crate) fn save_options(extension: &str) -> &'static str {
    match extension {
        "jpg" => "[Q=85,keep=icc]",
        "webp" => "[Q=80,keep=icc]",
        _ => "[keep=icc]",
    }
}

/// vips reads `[...]` after a filename as options, so a path containing one
/// would be misread. Our temp paths never do; refuse rather than guess.
pub(crate) fn vips_path(path: &Path) -> Result<String, MediaError> {
    let text = path
        .to_str()
        .ok_or_else(|| failed(format!("non-UTF-8 path {}", path.display())))?;
    if text.contains(['[', ']']) {
        return Err(failed(format!("path {text:?} contains a bracket")));
    }
    Ok(text.to_string())
}

pub(crate) fn sibling(path: &Path, infix: &str, extension: &str) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{stem}{infix}.{extension}"))
}

/// Describe a file just written.
fn describe(path: PathBuf, mime: &str, extension: &str) -> Result<DerivativeFile, MediaError> {
    let size = std::fs::metadata(&path)
        .map_err(|e| failed(format!("{} was not written: {e}", path.display())))?
        .len();
    let dims = imagesize::size(&path)
        .map_err(|e| failed(format!("{} is unreadable: {e}", path.display())))?;
    Ok(DerivativeFile {
        path,
        size,
        width: u32::try_from(dims.width).unwrap_or(u32::MAX),
        height: u32::try_from(dims.height).unwrap_or(u32::MAX),
        mime: mime.to_string(),
        extension: extension.to_string(),
    })
}

struct Thumbnail<'a> {
    source: &'a Path,
    keep_animation: bool,
    output: &'a Path,
    width: u32,
    height: u32,
    /// Crop to fill exactly `width` x `height`, enlarging if need be;
    /// otherwise fit inside and never enlarge.
    fill: bool,
}

async fn thumbnail(vips: &Tool<'_>, t: Thumbnail<'_>) -> Result<(), MediaError> {
    let load = if t.keep_animation { "[n=-1]" } else { "" };
    let extension = t
        .output
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("jpg");
    let mut args = vec![
        "thumbnail".to_string(),
        format!("{}{load}", vips_path(t.source)?),
        format!("{}{}", vips_path(t.output)?, save_options(extension)),
        t.width.to_string(),
        "--height".into(),
        t.height.to_string(),
    ];
    if t.fill && t.keep_animation {
        // libvips cannot crop an animation in thumbnail (it flattens it to
        // one frame), so an animated thumb fits inside the square instead
        // of filling it. Animated derivatives are opt-in.
        args.extend(["--size".into(), "both".into()]);
    } else if t.fill {
        args.extend([
            "--crop".into(),
            "centre".into(),
            "--size".into(),
            "both".into(),
        ]);
    } else {
        args.extend(["--size".into(), "down".into()]);
    }
    vips.run(args, failed).await.map(drop)
}

/// A still from a video: at one second, or a tenth of the way in for clips
/// shorter than ten seconds. ffmpeg applies the display rotation.
async fn extract_frame(
    ffmpeg: &Tool<'_>,
    source: &Path,
    duration_ms: Option<u32>,
    output: &Path,
) -> Result<(), MediaError> {
    let at = duration_ms.map_or(0.0, |ms| (f64::from(ms) / 1000.0 * 0.1).min(1.0));
    let input = format!("file:{}", source.display());
    let out = format!("file:{}", output.display());
    let args = |seek: f64| {
        vec![
            "-hide_banner".to_string(),
            "-loglevel".into(),
            "error".into(),
            "-nostdin".into(),
            "-protocol_whitelist".into(),
            "file".into(),
            "-ss".into(),
            format!("{seek:.3}"),
            "-i".into(),
            input.clone(),
            "-frames:v".into(),
            "1".into(),
            "-an".into(),
            "-q:v".into(),
            "3".into(),
            "-update".into(),
            "1".into(),
            "-y".into(),
            out.clone(),
        ]
    };
    ffmpeg.run(args(at), failed).await?;
    // Seeking past the last keyframe of a very short clip yields nothing;
    // the first frame is the fallback.
    if std::fs::metadata(output).map_or(true, |m| m.len() == 0) {
        ffmpeg.run(args(0.0), failed).await?;
    }
    Ok(())
}

pub(crate) async fn derive(
    config: &MediaConfig,
    path: &Path,
    probe: &ProbeResult,
    plan: &DerivativePlan,
) -> Result<DerivativeSet, MediaError> {
    let vips = Tool {
        binary: &config.vips,
        name: "vips",
        timeout: config.timeout,
    };
    let ffmpeg = Tool {
        binary: &config.ffmpeg,
        name: "ffmpeg",
        timeout: config.timeout,
    };

    let mut set = DerivativeSet::default();
    let size = std::fs::metadata(path)
        .map_err(|e| failed(format!("the upload vanished: {e}")))?
        .len();
    set.insert(
        Derivative::Image,
        DerivativeFile {
            path: path.to_path_buf(),
            size,
            width: probe.width,
            height: probe.height,
            mime: probe.mime.clone(),
            extension: probe.extension.clone(),
        },
    );
    if !probe.kind.has_visual_derivatives() {
        return Ok(set);
    }

    let (extension, mime) = probe.kind.derivative_format(&probe.extension);
    let (source, width, height, animated) = if probe.kind.needs_frame() {
        let frame_path = sibling(path, Derivative::Frame.suffix(), "jpg");
        extract_frame(&ffmpeg, path, probe.duration_ms, &frame_path).await?;
        let frame = describe(frame_path, "image/jpeg", "jpg")?;
        let found = (frame.path.clone(), frame.width, frame.height, false);
        set.insert(Derivative::Frame, frame);
        found
    } else {
        (
            path.to_path_buf(),
            probe.width,
            probe.height,
            probe.is_animated,
        )
    };
    let keep_animation = animated && plan.animated_derivatives;

    let thumb = sibling(path, Derivative::Thumb.suffix(), extension);
    thumbnail(
        &vips,
        Thumbnail {
            source: &source,
            keep_animation,
            output: &thumb,
            width: plan.thumb_edge,
            height: plan.thumb_edge,
            fill: true,
        },
    )
    .await?;
    set.insert(Derivative::Thumb, describe(thumb, mime, extension)?);

    if plan.wants_medium(width, height) {
        let (w, h) = match plan.medium_dimension {
            MediumDimension::Width => (plan.medium_edge, UNBOUNDED),
            MediumDimension::Height => (UNBOUNDED, plan.medium_edge),
            MediumDimension::Longest => (plan.medium_edge, plan.medium_edge),
        };
        let medium = sibling(path, Derivative::Medium.suffix(), extension);
        thumbnail(
            &vips,
            Thumbnail {
                source: &source,
                keep_animation,
                output: &medium,
                width: w,
                height: h,
                fill: false,
            },
        )
        .await?;
        set.insert(Derivative::Medium, describe(medium, mime, extension)?);
    }
    Ok(set)
}

/// Shrink the served file to `max_width`, upright. Returns `path` itself
/// when there is nothing to do: already narrow enough, or animated — which
/// upstream does not resize either.
pub(crate) async fn resize(
    config: &MediaConfig,
    path: &Path,
    probe: &ProbeResult,
    max_width: u32,
) -> Result<PathBuf, MediaError> {
    if probe.kind != MediaKind::Image || probe.is_animated || probe.width <= max_width {
        return Ok(path.to_path_buf());
    }
    let vips = Tool {
        binary: &config.vips,
        name: "vips",
        timeout: config.timeout,
    };
    let (extension, _) = probe.kind.derivative_format(&probe.extension);
    let output = sibling(path, ".resized", extension);
    thumbnail(
        &vips,
        Thumbnail {
            source: path,
            keep_animation: false,
            output: &output,
            width: max_width,
            height: UNBOUNDED,
            fill: false,
        },
    )
    .await?;
    Ok(output)
}
