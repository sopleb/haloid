//! What inspecting an uploaded file tells us about it.

use super::kind::MediaKind;

/// The result of probing a file: libvips for images, ffprobe for video and
/// audio. Everything here is read off the bytes, never off what the client
/// claimed.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    pub kind: MediaKind,
    /// Sniffed from magic bytes.
    pub mime: String,
    /// Canonical extension for the sniffed type, without a dot.
    pub extension: String,
    /// Post-rotation width. Zero for audio.
    pub width: u32,
    /// Post-rotation height. Zero for audio.
    pub height: u32,
    /// Duration for video and audio.
    pub duration_ms: Option<u32>,
    pub is_animated: bool,
    /// Frame count for animated images, where libvips reports it.
    pub frame_count: Option<u32>,
}

impl ProbeResult {
    /// Total pixel count, for the decompression-bomb guard. Saturates rather
    /// than overflowing on absurd dimensions.
    pub fn pixels(&self) -> u64 {
        u64::from(self.width).saturating_mul(u64::from(self.height))
    }

    /// Aspect ratio, as the legacy API's `ratio` field. Guards division by
    /// zero: audio and malformed dimensions report 1.0 rather than NaN or inf.
    pub fn ratio(&self) -> f64 {
        if self.height == 0 {
            return 1.0;
        }
        f64::from(self.width) / f64::from(self.height)
    }

    /// The longest edge, which is what the medium-derivative threshold is
    /// measured against.
    pub fn longest_edge(&self) -> u32 {
        self.width.max(self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32) -> ProbeResult {
        ProbeResult {
            kind: MediaKind::Image,
            mime: "image/jpeg".into(),
            extension: "jpg".into(),
            width,
            height,
            duration_ms: None,
            is_animated: false,
            frame_count: None,
        }
    }

    #[test]
    fn pixels_saturates_instead_of_overflowing() {
        assert_eq!(image(1920, 1080).pixels(), 2_073_600);
        assert_eq!(
            image(u32::MAX, u32::MAX).pixels(),
            u64::from(u32::MAX) * u64::from(u32::MAX)
        );
    }

    #[test]
    fn ratio_never_returns_nan_or_infinity() {
        assert_eq!(image(1000, 500).ratio(), 2.0);
        assert_eq!(image(0, 0).ratio(), 1.0);
        assert_eq!(image(1000, 0).ratio(), 1.0);
        assert!(image(0, 1000).ratio().is_finite());
    }

    #[test]
    fn longest_edge_ignores_orientation() {
        assert_eq!(image(800, 600).longest_edge(), 800);
        assert_eq!(image(600, 800).longest_edge(), 800);
        assert_eq!(image(500, 500).longest_edge(), 500);
    }
}
