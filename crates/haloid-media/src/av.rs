//! Probing video and audio with ffprobe.
//!
//! ffprobe parses untrusted input, which makes it one of the two
//! highest-severity surfaces in the system. So it runs under a wall-clock
//! timeout and is killed when that fires, may only open the one local file
//! (`-protocol_whitelist file`, and the path passed as `file:` so a name
//! like `http:x` is not a URL), and its own error text never reaches a
//! client — `MediaError` is not client-safe. Memory caps and a sandbox
//! belong to the deploy (chunk 22).

use crate::sniff::Sniffed;
use haloid_core::MediaError;
use haloid_core::media::{MediaKind, ProbeResult};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    streams: Vec<Stream>,
    format: Option<Format>,
}

#[derive(Deserialize)]
struct Stream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<String>,
    #[serde(default)]
    side_data_list: Vec<serde_json::Value>,
    #[serde(default)]
    tags: HashMap<String, String>,
    #[serde(default)]
    disposition: HashMap<String, i64>,
}

#[derive(Deserialize)]
struct Format {
    duration: Option<String>,
}

pub(crate) async fn probe(
    ffprobe: &Path,
    timeout: Duration,
    path: &Path,
    sniffed: Sniffed,
) -> Result<ProbeResult, MediaError> {
    let target = format!("file:{}", path.display());
    let tool = crate::tool::Tool {
        binary: ffprobe,
        name: "ffprobe",
        timeout,
    };
    let stdout = tool
        .run(
            [
                "-v",
                "error",
                "-hide_banner",
                "-protocol_whitelist",
                "file",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
                "-i",
                &target,
            ],
            MediaError::ProbeFailed,
        )
        .await?;
    let parsed: Output = serde_json::from_slice(&stdout)
        .map_err(|e| MediaError::ProbeFailed(format!("unreadable ffprobe output: {e}")))?;
    interpret(&parsed, sniffed)
}

fn interpret(out: &Output, sniffed: Sniffed) -> Result<ProbeResult, MediaError> {
    let is = |s: &&Stream, kind: &str| s.codec_type.as_deref() == Some(kind);
    // Cover art in an MP3 or M4A is a "video" stream marked as an attached
    // picture. It does not make the file a video.
    let video = out
        .streams
        .iter()
        .find(|s| is(s, "video") && s.disposition.get("attached_pic") != Some(&1));
    let audio = out.streams.iter().find(|s| is(s, "audio"));

    let kind = match (sniffed.kind, video, audio) {
        (MediaKind::Video, Some(_), _) => MediaKind::Video,
        // An MP4 container holding only sound is audio, whatever its brand.
        (MediaKind::Video | MediaKind::Audio, None, Some(_)) => MediaKind::Audio,
        (MediaKind::Audio, Some(_), _) => MediaKind::Audio,
        _ => return Err(MediaError::ProbeFailed("no playable stream".into())),
    };

    let duration_ms = out
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .or_else(|| video.or(audio).and_then(|s| s.duration.as_deref()))
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d >= 0.0)
        .map(|secs| (secs * 1000.0).round().min(f64::from(u32::MAX)) as u32);

    let (mut width, mut height) = match (kind, video) {
        (MediaKind::Video, Some(v)) => (v.width.unwrap_or(0), v.height.unwrap_or(0)),
        _ => (0, 0),
    };
    if kind == MediaKind::Video && video.is_some_and(|v| matches!(rotation(v), 90 | 270)) {
        std::mem::swap(&mut width, &mut height);
    }

    let (mime, extension) = if kind == MediaKind::Audio && sniffed.kind == MediaKind::Video {
        ("audio/mp4", "m4a")
    } else {
        (sniffed.mime, sniffed.extension)
    };

    Ok(ProbeResult {
        kind,
        mime: mime.to_string(),
        extension: extension.to_string(),
        width,
        height,
        duration_ms,
        is_animated: false,
        frame_count: None,
    })
}

/// The display rotation in degrees, normalised to 0..360: from the display
/// matrix side data in current ffmpeg, or the older `rotate` tag.
fn rotation(stream: &Stream) -> i64 {
    let from_side_data = stream
        .side_data_list
        .iter()
        .find_map(|d| d.get("rotation").and_then(serde_json::Value::as_f64));
    let from_tag = stream
        .tags
        .get("rotate")
        .and_then(|r| r.parse::<f64>().ok());
    let degrees = from_side_data.or(from_tag).unwrap_or(0.0).round() as i64;
    degrees.rem_euclid(360)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(json: &str) -> Stream {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn rotation_reads_both_spellings_and_normalises() {
        assert_eq!(
            rotation(&stream(r#"{"side_data_list":[{"rotation":-90}]}"#)),
            270
        );
        assert_eq!(
            rotation(&stream(r#"{"side_data_list":[{"rotation":90}]}"#)),
            90
        );
        assert_eq!(rotation(&stream(r#"{"tags":{"rotate":"180"}}"#)), 180);
        assert_eq!(rotation(&stream("{}")), 0);
    }

    #[test]
    fn cover_art_does_not_make_audio_a_video() {
        let out: Output = serde_json::from_str(
            r#"{"streams":[
                {"codec_type":"audio"},
                {"codec_type":"video","width":500,"height":500,"disposition":{"attached_pic":1}}
              ],"format":{"duration":"3.5"}}"#,
        )
        .unwrap();
        let sniffed = crate::sniff::sniff(b"ID3\x04\x00\x00\x00\x00\x00\x00").unwrap();
        let probe = interpret(&out, sniffed).unwrap();
        assert_eq!(probe.kind, MediaKind::Audio);
        assert_eq!((probe.width, probe.height), (0, 0));
        assert_eq!(probe.duration_ms, Some(3500));
    }
}
