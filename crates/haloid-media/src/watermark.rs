//! Stamping the operator's watermark on an image, an animation or a video.
//!
//! The mark is made once per file: scaled to a share of the picture's width,
//! given an alpha channel if it has none, and faded to the opacity asked for.
//! Then:
//!
//! - **A still** is turned upright first (a mark composited onto unrotated
//!   pixels would land in the wrong corner, sideways, once a viewer applied
//!   the orientation tag), and the mark laid over it with vips.
//! - **An animated GIF or WebP** is loaded whole, as vips does, a strip of
//!   frames one above another; a strip of marks, one per frame, is laid over
//!   it, so every frame is marked and the timing is kept. Animated PNG is
//!   left alone: vips reads only its first frame.
//! - **A video** gets the mark overlaid on every frame by ffmpeg. The picture
//!   has to be re-encoded for that (same codec family as it came in: H.264
//!   for MP4 and MOV, VP9 for WebM); the sound is copied untouched. ffmpeg
//!   applies the display rotation, so the mark is placed as the video is
//!   seen.
//!
//! Intermediates are uncompressed TIFF, never re-encoded lossily.

use crate::derive::{UNBOUNDED, failed, save_options, sibling, vips_path};
use crate::tool::Tool;
use haloid_core::MediaError;
use haloid_core::media::{MediaKind, ProbeResult};
use haloid_core::processor::Watermark;
use std::path::{Path, PathBuf};

struct Header {
    width: u32,
    height: u32,
    bands: u32,
    /// Frames, for an animation; 1 otherwise.
    pages: u32,
}

/// `vipsheader`, beside `vips`: the CLI's header reader is its own binary.
fn vipsheader(vips: &Tool<'_>) -> PathBuf {
    match vips.binary.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => dir.join("vipsheader"),
        None => PathBuf::from("vipsheader"),
    }
}

async fn header(vips: &Tool<'_>, path: &Path) -> Result<Header, MediaError> {
    let binary = vipsheader(vips);
    let reader = Tool {
        binary: &binary,
        name: "vipsheader",
        timeout: vips.timeout,
    };
    let field = |name: &'static str| {
        let path = path.to_path_buf();
        let reader = &reader;
        async move {
            let out = reader.run(["-f", name, &vips_path(&path)?], failed).await?;
            String::from_utf8_lossy(&out)
                .trim()
                .parse::<u32>()
                .map_err(|_| failed(format!("vips header gave no {name}")))
        }
    };
    Ok(Header {
        width: field("width").await?,
        height: field("height").await?,
        bands: field("bands").await?,
        pages: field("n-pages").await.unwrap_or(1).max(1),
    })
}

/// Whether this file takes a watermark at all.
pub(crate) fn applies(probe: &ProbeResult, watermark: &Watermark) -> bool {
    let kind = match probe.kind {
        MediaKind::Video => true,
        MediaKind::Image => !probe.is_animated || animation_format(&probe.extension),
        MediaKind::Audio => false,
    };
    kind && probe.width >= watermark.min_width && probe.height >= watermark.min_height
}

/// Animations vips can load whole and write back as animations.
fn animation_format(extension: &str) -> bool {
    matches!(extension, "gif" | "webp")
}

pub(crate) async fn stamp(
    vips: &Tool<'_>,
    ffmpeg: &Tool<'_>,
    path: &Path,
    probe: &ProbeResult,
    watermark: &Watermark,
) -> Result<PathBuf, MediaError> {
    if !applies(probe, watermark) {
        return Ok(path.to_path_buf());
    }
    // Uncompressed TIFF between steps: lossless and quick, and a loader
    // libvips trusts — its own `.v` format is refused under
    // VIPS_BLOCK_UNTRUSTED, which every run sets.
    let work = |infix: &str| sibling(path, infix, "tif");
    let mut temps = vec![
        work(".wm-scaled"),
        work(".wm-alpha"),
        work(".wm-faded"),
        work(".wm-mark"),
    ];
    let result = async {
        match probe.kind {
            MediaKind::Video => {
                let mark = prepare_mark(vips, watermark, probe.width, &temps).await?;
                let png = sibling(path, ".wm-mark", "png");
                temps.push(png.clone());
                vips.run(["copy", &vips_path(&mark.path)?, &vips_path(&png)?], failed)
                    .await?;
                video(
                    ffmpeg,
                    path,
                    probe,
                    watermark,
                    &png,
                    (mark.width, mark.height),
                )
                .await
            }
            _ if probe.is_animated => {
                let base = header(vips, path).await?;
                let mark = prepare_mark(vips, watermark, base.width, &temps).await?;
                let (page, strip) = (work(".wm-page"), work(".wm-strip"));
                temps.extend([page.clone(), strip.clone()]);
                animation(vips, path, probe, watermark, &base, &mark, &page, &strip).await
            }
            _ => {
                let (upright, laid, flat) =
                    (work(".wm-upright"), work(".wm-laid"), work(".wm-flat"));
                temps.extend([upright.clone(), laid.clone(), flat.clone()]);
                vips.run(
                    ["autorot", &vips_path(path)?, &vips_path(&upright)?],
                    failed,
                )
                .await?;
                let base = header(vips, &upright).await?;
                let mark = prepare_mark(vips, watermark, base.width, &temps).await?;
                still(
                    vips, path, probe, watermark, &base, &mark, &upright, &laid, &flat,
                )
                .await
            }
        }
    }
    .await;
    for file in &temps {
        let _ = tokio::fs::remove_file(file).await;
    }
    result
}

/// The mark, ready to lay: four bands, faded, at the right width.
struct Mark {
    path: PathBuf,
    width: u32,
    height: u32,
}

/// Scale the operator's mark to its share of `base_width`, give it alpha,
/// and fade it. `temps` are the four working files, in order.
async fn prepare_mark(
    vips: &Tool<'_>,
    watermark: &Watermark,
    base_width: u32,
    temps: &[PathBuf],
) -> Result<Mark, MediaError> {
    let [scaled, alpha, faded, mark] = [&temps[0], &temps[1], &temps[2], &temps[3]];
    let mark_width = (base_width * u32::from(watermark.width_percent.clamp(1, 100)) / 100).max(1);
    vips.run(
        [
            "thumbnail".to_string(),
            vips_path(&watermark.mark)?,
            vips_path(scaled)?,
            mark_width.to_string(),
            "--height".into(),
            UNBOUNDED.to_string(),
            "--size".into(),
            "both".into(),
        ],
        failed,
    )
    .await?;
    let scaled_header = header(vips, scaled).await?;
    let mut bands = scaled_header.bands;
    let with_alpha = if bands == 1 || bands == 3 {
        vips.run(
            [
                "bandjoin_const",
                &vips_path(scaled)?,
                &vips_path(alpha)?,
                "255",
            ],
            failed,
        )
        .await?;
        bands += 1;
        alpha
    } else {
        scaled
    };

    // Scale only the alpha band: ones for colour, the opacity for alpha.
    let opacity = f64::from(watermark.opacity_percent.min(100)) / 100.0;
    let mut multipliers = vec!["1".to_string(); bands.saturating_sub(1) as usize];
    multipliers.push(format!("{opacity}"));
    let offsets = vec!["0".to_string(); bands as usize];
    vips.run(
        [
            "linear".to_string(),
            vips_path(with_alpha)?,
            vips_path(faded)?,
            multipliers.join(" "),
            offsets.join(" "),
        ],
        failed,
    )
    .await?;
    vips.run(
        ["cast", &vips_path(faded)?, &vips_path(mark)?, "uchar"],
        failed,
    )
    .await?;
    Ok(Mark {
        path: mark.clone(),
        width: scaled_header.width,
        height: scaled_header.height,
    })
}

#[allow(clippy::too_many_arguments)]
async fn still(
    vips: &Tool<'_>,
    path: &Path,
    probe: &ProbeResult,
    watermark: &Watermark,
    base: &Header,
    mark: &Mark,
    upright: &Path,
    laid: &Path,
    flat: &Path,
) -> Result<PathBuf, MediaError> {
    let (x, y) = watermark.position.place(
        (base.width, base.height),
        (mark.width, mark.height),
        watermark.margin,
    );
    vips.run(
        [
            "composite2".to_string(),
            vips_path(upright)?,
            vips_path(&mark.path)?,
            vips_path(laid)?,
            "over".into(),
            "--x".into(),
            x.to_string(),
            "--y".into(),
            y.to_string(),
        ],
        failed,
    )
    .await?;

    // An opaque image stays opaque: composite added an alpha channel.
    let final_source = if base.bands == 1 || base.bands == 3 {
        vips.run(["flatten", &vips_path(laid)?, &vips_path(flat)?], failed)
            .await?;
        flat
    } else {
        laid
    };
    let (extension, _) = probe.kind.derivative_format(&probe.extension);
    let output = sibling(path, ".watermarked", extension);
    vips.run(
        [
            "copy".to_string(),
            vips_path(final_source)?,
            format!("{}{}", vips_path(&output)?, save_options(extension)),
        ],
        failed,
    )
    .await?;
    Ok(output)
}

/// Every frame of a GIF or WebP: the frames are one tall strip to vips, so a
/// strip of marks, one per frame at the same place in each, goes over it in
/// one composite, and the frame height, timing and loop are carried through.
#[allow(clippy::too_many_arguments)]
async fn animation(
    vips: &Tool<'_>,
    path: &Path,
    probe: &ProbeResult,
    watermark: &Watermark,
    base: &Header,
    mark: &Mark,
    page: &Path,
    strip: &Path,
) -> Result<PathBuf, MediaError> {
    let (x, y) = watermark.position.place(
        (base.width, base.height),
        (mark.width, mark.height),
        watermark.margin,
    );
    vips.run(
        [
            "embed".to_string(),
            vips_path(&mark.path)?,
            vips_path(page)?,
            x.to_string(),
            y.to_string(),
            base.width.to_string(),
            base.height.to_string(),
            "--extend".into(),
            "background".into(),
            "--background".into(),
            "0 0 0 0".into(),
        ],
        failed,
    )
    .await?;
    vips.run(
        [
            "replicate".to_string(),
            vips_path(page)?,
            vips_path(strip)?,
            "1".into(),
            base.pages.to_string(),
        ],
        failed,
    )
    .await?;
    let output = sibling(path, ".watermarked", &probe.extension);
    vips.run(
        [
            "composite2".to_string(),
            format!("{}[n=-1]", vips_path(path)?),
            vips_path(strip)?,
            format!("{}{}", vips_path(&output)?, save_options(&probe.extension)),
            "over".into(),
        ],
        failed,
    )
    .await?;
    Ok(output)
}

/// Every frame of a video, with ffmpeg: the picture re-encoded, the sound
/// copied.
async fn video(
    ffmpeg: &Tool<'_>,
    path: &Path,
    probe: &ProbeResult,
    watermark: &Watermark,
    mark: &Path,
    mark_size: (u32, u32),
) -> Result<PathBuf, MediaError> {
    let (x, y) = watermark
        .position
        .place((probe.width, probe.height), mark_size, watermark.margin);
    let output = sibling(path, ".watermarked", &probe.extension);
    // Overlay, then round the size to even numbers, which 4:2:0 needs.
    let filter = format!(
        "[0:v][1:v]overlay={x}:{y}:format=auto,pad=ceil(iw/2)*2:ceil(ih/2)*2,format=yuv420p[v]"
    );
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-protocol_whitelist",
        "file",
        "-i",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    args.push(format!("file:{}", path.display()));
    args.extend(["-i".into(), format!("file:{}", mark.display())]);
    args.extend(
        [
            "-filter_complex",
            &filter,
            "-map",
            "[v]",
            "-map",
            "0:a?",
            "-c:a",
            "copy",
        ]
        .iter()
        .map(|s| (*s).to_string()),
    );
    let codec: &[&str] = if probe.extension == "webm" {
        &[
            "-c:v",
            "libvpx-vp9",
            "-crf",
            "32",
            "-b:v",
            "0",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
            "-row-mt",
            "1",
        ]
    } else {
        &[
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-movflags",
            "+faststart",
        ]
    };
    args.extend(codec.iter().map(|s| (*s).to_string()));
    args.extend(["-y".into(), format!("file:{}", output.display())]);
    ffmpeg.run(args, failed).await?;
    Ok(output)
}
