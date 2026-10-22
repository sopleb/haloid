//! Removing identifying metadata from the file we serve.
//!
//! Done once, at upload — never per request, which would cost static
//! serving, range requests and CDN caching. Wherever possible it is
//! lossless: JPEG, PNG and WebP are rewritten at the container level,
//! dropping metadata segments and chunks and copying the image data
//! untouched. Only orientation and the colour profile survive, because
//! display needs them. Video and audio are remuxed with every tag and
//! non-media stream dropped. HEIF and AVIF are re-encoded by libvips, the
//! one lossy case.
//!
//! Every rewrite goes to a sibling file renamed over the original, so a
//! failure leaves the upload as it was.

use crate::MediaConfig;
use crate::exif::{orientation, orientation_only};
use crate::tool::Tool;
use haloid_core::MediaError;
use haloid_core::media::{MediaKind, ProbeResult};
use std::path::{Path, PathBuf};

fn failed(reason: String) -> MediaError {
    MediaError::DeriveFailed(reason)
}

fn io(e: std::io::Error) -> MediaError {
    failed(format!("could not rewrite the upload: {e}"))
}

fn temp_sibling(path: &Path, extension: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.stripping.{extension}"))
}

fn replace(path: &Path, bytes: &[u8]) -> Result<(), MediaError> {
    let temp = temp_sibling(path, "tmp");
    std::fs::write(&temp, bytes).map_err(io)?;
    std::fs::rename(&temp, path).map_err(io)
}

pub(crate) async fn strip(
    config: &MediaConfig,
    path: &Path,
    probe: &ProbeResult,
) -> Result<(), MediaError> {
    match (probe.kind, probe.extension.as_str()) {
        (MediaKind::Image, "jpg" | "png" | "webp") => {
            let owned = path.to_path_buf();
            let extension = probe.extension.clone();
            tokio::task::spawn_blocking(move || {
                let bytes = std::fs::read(&owned).map_err(io)?;
                let keep = orientation(&owned);
                let stripped = match extension.as_str() {
                    "jpg" => jpeg(&bytes, keep)?,
                    "png" => png(&bytes, keep)?,
                    _ => webp(&bytes, keep)?,
                };
                replace(&owned, &stripped)
            })
            .await
            .map_err(|e| failed(e.to_string()))?
        }
        // GIF has no EXIF, and its comment blocks are vanishingly rare.
        (MediaKind::Image, "gif") => Ok(()),
        (MediaKind::Image, _) => reencode(config, path, &probe.extension).await,
        (MediaKind::Video | MediaKind::Audio, _) => remux(config, path, &probe.extension).await,
    }
}

/// JPEG: keep JFIF (APP0), the ICC profile (APP2) and Adobe's colour
/// transform (APP14) — the last decides how the pixels decode — and drop
/// every other APPn and every comment. A non-upright orientation is
/// re-added as a minimal EXIF block. Everything from start-of-scan on is
/// copied byte for byte.
fn jpeg(bytes: &[u8], orientation: u32) -> Result<Vec<u8>, MediaError> {
    let bad = || failed("malformed JPEG".into());
    if bytes.get(0..2) != Some(&[0xFF, 0xD8]) {
        return Err(bad());
    }
    let mut out = vec![0xFF, 0xD8];
    if orientation != 1 {
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(orientation_only(orientation));
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend(app1);
    }

    let mut at = 2;
    loop {
        if bytes.get(at) != Some(&0xFF) {
            return Err(bad());
        }
        let marker = *bytes.get(at + 1).ok_or_else(bad)?;
        // Fill bytes between markers.
        if marker == 0xFF {
            at += 1;
            continue;
        }
        // Start of scan: the rest is image data, copied untouched.
        if marker == 0xDA {
            out.extend_from_slice(&bytes[at..]);
            return Ok(out);
        }
        let len = usize::from(u16::from_be_bytes([
            *bytes.get(at + 2).ok_or_else(bad)?,
            *bytes.get(at + 3).ok_or_else(bad)?,
        ]));
        let end = at + 2 + len;
        let segment = bytes.get(at..end).ok_or_else(bad)?;
        let drop = match marker {
            0xE0 | 0xE2 | 0xEE => false,
            0xE1..=0xEF | 0xFE => true,
            _ => false,
        };
        if !drop {
            out.extend_from_slice(segment);
        }
        at = end;
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
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

/// PNG: drop text, time and EXIF chunks; keep everything else, colour
/// profile included. A non-upright orientation is re-added as a minimal
/// `eXIf` chunk after the header.
fn png(bytes: &[u8], orientation: u32) -> Result<Vec<u8>, MediaError> {
    let bad = || failed("malformed PNG".into());
    let signature = bytes.get(..8).ok_or_else(bad)?;
    let mut out = signature.to_vec();
    let mut at = 8;
    while at < bytes.len() {
        let len = u32::from_be_bytes(bytes.get(at..at + 4).ok_or_else(bad)?.try_into().unwrap());
        let end = at + 12 + len as usize;
        let chunk = bytes.get(at..end).ok_or_else(bad)?;
        let kind = &chunk[4..8];
        if !matches!(kind, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf" | b"tIME") {
            out.extend_from_slice(chunk);
        }
        if kind == b"IHDR" && orientation != 1 {
            let data = orientation_only(orientation);
            let mut body = b"eXIf".to_vec();
            body.extend(&data);
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc32(&body).to_be_bytes());
        }
        at = end;
    }
    Ok(out)
}

/// WebP: drop the `EXIF` and `XMP ` chunks and clear their flags in the
/// `VP8X` header; re-add a minimal `EXIF` for a non-upright orientation.
fn webp(bytes: &[u8], orientation: u32) -> Result<Vec<u8>, MediaError> {
    let bad = || failed("malformed WebP".into());
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return Err(bad());
    }
    let mut chunks = Vec::new();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let end = at + 8 + len + (len & 1);
        let chunk = bytes.get(at..end.min(bytes.len())).ok_or_else(bad)?;
        if !matches!(&chunk[..4], b"EXIF" | b"XMP ") {
            chunks.push(chunk.to_vec());
        }
        at = end;
    }

    let extended = chunks.iter().any(|c| &c[..4] == b"VP8X");
    let keep_orientation = orientation != 1 && extended;
    for chunk in &mut chunks {
        if &chunk[..4] == b"VP8X" {
            // Flags: 0x08 EXIF, 0x04 XMP.
            chunk[8] &= !0x0C;
            if keep_orientation {
                chunk[8] |= 0x08;
            }
        }
    }
    if keep_orientation {
        let data = orientation_only(orientation);
        let mut chunk = b"EXIF".to_vec();
        chunk.extend_from_slice(&(data.len() as u32).to_le_bytes());
        chunk.extend(&data);
        if data.len() % 2 == 1 {
            chunk.push(0);
        }
        chunks.push(chunk);
    }

    let body: Vec<u8> = chunks.concat();
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend(body);
    Ok(out)
}

/// HEIF and AVIF: re-encoded by libvips with only the colour profile kept.
/// The decoder has already applied the container's rotation, so the result
/// is upright. Lossy, and the one place stripping changes pixels.
async fn reencode(config: &MediaConfig, path: &Path, extension: &str) -> Result<(), MediaError> {
    let temp = temp_sibling(path, extension);
    let compression = if extension == "avif" { "av1" } else { "hevc" };
    let vips = Tool {
        binary: &config.vips,
        name: "vips",
        timeout: config.timeout,
    };
    let args = [
        "heifsave".to_string(),
        path.display().to_string(),
        temp.display().to_string(),
        "--compression".into(),
        compression.into(),
        "--keep".into(),
        "icc".into(),
    ];
    if let Err(e) = vips.run(args, failed).await {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    std::fs::rename(&temp, path).map_err(io)
}

/// Video and audio: remux with every global and per-stream tag and every
/// chapter dropped, and only the video and audio streams kept — a camera's
/// GPS track is a data stream. Stream copy, so nothing is re-encoded; the
/// display rotation lives in the stream itself and survives.
async fn remux(config: &MediaConfig, path: &Path, extension: &str) -> Result<(), MediaError> {
    let format = match extension {
        "mov" => "mov",
        "webm" => "webm",
        "mp3" => "mp3",
        "wav" => "wav",
        _ => "mp4",
    };
    let temp = temp_sibling(path, extension);
    let ffmpeg = Tool {
        binary: &config.ffmpeg,
        name: "ffmpeg",
        timeout: config.timeout,
    };
    let args = [
        "-hide_banner".to_string(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        "-protocol_whitelist".into(),
        "file".into(),
        "-i".into(),
        format!("file:{}", path.display()),
        "-map".into(),
        "0:v?".into(),
        "-map".into(),
        "0:a?".into(),
        "-map_metadata".into(),
        "-1".into(),
        "-map_metadata:s".into(),
        "-1".into(),
        "-map_chapters".into(),
        "-1".into(),
        "-c".into(),
        "copy".into(),
        "-f".into(),
        format.into(),
        "-y".into(),
        format!("file:{}", temp.display()),
    ];
    if let Err(e) = ffmpeg.run(args, failed).await {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    std::fs::rename(&temp, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jpeg_without_its_start_marker_is_refused() {
        assert!(jpeg(b"not a jpeg", 1).is_err());
        assert!(jpeg(&[0xFF, 0xD8, 0xFF], 1).is_err(), "truncated");
    }

    #[test]
    fn a_webp_keeps_its_orientation_only_where_it_can() {
        // A simple (lossy, no VP8X) WebP cannot carry EXIF at all.
        let mut simple = b"RIFF\0\0\0\0WEBPVP8 \x04\0\0\0abcd".to_vec();
        let len = (simple.len() - 8) as u32;
        simple[4..8].copy_from_slice(&len.to_le_bytes());
        let out = webp(&simple, 6).unwrap();
        assert_eq!(out, simple);
    }
}
