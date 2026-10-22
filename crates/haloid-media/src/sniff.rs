//! Deciding what a file really is, from its bytes.
//!
//! The client's filename and `Content-Type` are never consulted. Magic bytes
//! pick the type; anything outside the accepted set is refused here, before
//! a decoder or ffprobe ever sees it.

use haloid_core::MediaError;
use haloid_core::media::MediaKind;

/// How many leading bytes sniffing reads. Enough for every signature we
/// accept, and for an SVG's root element behind a long XML prologue.
pub(crate) const HEAD_LEN: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sniffed {
    pub kind: MediaKind,
    /// The type we serve the file as. Canonical, not whatever `infer` calls
    /// it: `audio/wav`, not `audio/x-wav`.
    pub mime: &'static str,
    pub extension: &'static str,
}

const fn sniffed(kind: MediaKind, mime: &'static str, extension: &'static str) -> Sniffed {
    Sniffed {
        kind,
        mime,
        extension,
    }
}

/// Identify a file from its first [`HEAD_LEN`] bytes.
pub(crate) fn sniff(head: &[u8]) -> Result<Sniffed, MediaError> {
    // First, before anything else: SVG is script served from our own
    // origin — stored XSS — and some image libraries will happily rasterise
    // it, so it must never reach one.
    if looks_like_svg(head) {
        return Err(MediaError::Unsupported("SVG is not accepted".into()));
    }

    let detected =
        infer::get(head).ok_or_else(|| MediaError::Unsupported("unrecognised file type".into()))?;
    use MediaKind::{Audio, Image, Video};
    Ok(match detected.mime_type() {
        "image/jpeg" => sniffed(Image, "image/jpeg", "jpg"),
        "image/png" => sniffed(Image, "image/png", "png"),
        "image/apng" => sniffed(Image, "image/apng", "png"),
        "image/gif" => sniffed(Image, "image/gif", "gif"),
        "image/webp" => sniffed(Image, "image/webp", "webp"),
        "image/avif" => sniffed(Image, "image/avif", "avif"),
        "image/heif" | "image/heic" => heif_flavour(head),
        "video/mp4" => sniffed(Video, "video/mp4", "mp4"),
        "video/quicktime" => sniffed(Video, "video/quicktime", "mov"),
        "video/webm" => sniffed(Video, "video/webm", "webm"),
        "audio/mpeg" => sniffed(Audio, "audio/mpeg", "mp3"),
        "audio/x-wav" | "audio/wav" => sniffed(Audio, "audio/wav", "wav"),
        "audio/m4a" | "audio/mp4" => sniffed(Audio, "audio/mp4", "m4a"),
        other => return Err(MediaError::Unsupported(other.to_string())),
    })
}

/// HEIC (HEVC-coded HEIF, what phones produce) versus other HEIF, told
/// apart by the `ftyp` major brand.
fn heif_flavour(head: &[u8]) -> Sniffed {
    match head.get(8..12) {
        Some(b"heic" | b"heix" | b"hevc" | b"hevx") => {
            sniffed(MediaKind::Image, "image/heic", "heic")
        }
        _ => sniffed(MediaKind::Image, "image/heif", "heif"),
    }
}

/// Text that opens like XML or contains an `<svg` element. Deliberately
/// broad: a false positive refuses a file that was never an accepted image
/// type anyway, while a false negative is a stored XSS.
fn looks_like_svg(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let start = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(head.len());
    let text = head[start..].to_ascii_lowercase();
    text.starts_with(b"<?xml")
        || text.starts_with(b"<svg")
        || text.starts_with(b"<!doctype svg")
        || text.windows(4).any(|w| w == b"<svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_is_caught_in_its_disguises() {
        for svg in [
            &b"<svg xmlns='http://www.w3.org/2000/svg'/>"[..],
            b"  \n<?xml version='1.0'?><svg/>",
            b"\xEF\xBB\xBF<svg/>",
            b"<!DOCTYPE svg PUBLIC '-//W3C//DTD SVG 1.1//EN'><svg/>",
            b"<!-- a comment first --><SVG onload=alert(1)>",
        ] {
            assert!(
                matches!(sniff(svg), Err(MediaError::Unsupported(r)) if r.contains("SVG")),
                "{:?}",
                String::from_utf8_lossy(svg)
            );
        }
    }

    #[test]
    fn unknown_and_unaccepted_types_are_refused() {
        assert!(sniff(b"").is_err());
        assert!(sniff(b"plain text").is_err());
        // A real signature, but not a type we host.
        assert!(matches!(
            sniff(b"%PDF-1.7\n"),
            Err(MediaError::Unsupported(m)) if m == "application/pdf"
        ));
    }
}
