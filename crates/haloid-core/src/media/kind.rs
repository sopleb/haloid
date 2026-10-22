//! The three media kinds we accept.

use std::fmt;
use std::str::FromStr;

/// What sort of file this is.
///
/// Documents are deliberately absent: the legacy API reports them as
/// `"image"` anyway, and we do not accept them at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}

impl MediaKind {
    /// The wire representation, identical in both API surfaces.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    /// Whether a still frame has to be extracted before derivatives can be
    /// made. Video derives its thumb and medium from an extracted frame, not
    /// from the video itself.
    pub const fn needs_frame(self) -> bool {
        matches!(self, Self::Video)
    }

    /// Whether this kind gets visual derivatives at all. Audio does not: we do
    /// not invent artwork for it, so it carries only the served file
    /// (`chain = 4`).
    pub const fn has_visual_derivatives(self) -> bool {
        matches!(self, Self::Image | Self::Video)
    }

    pub const ALL: [MediaKind; 3] = [Self::Image, Self::Video, Self::Audio];

    /// The kind a served extension belongs to, for the formats we accept.
    /// The upload itself is judged by its bytes; this is for telling a
    /// client in advance, such as which extensions an edition takes.
    pub fn of_extension(extension: &str) -> Option<Self> {
        Some(match extension.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "heic" | "heif" => Self::Image,
            "mp4" | "mov" | "webm" => Self::Video,
            "mp3" | "m4a" | "wav" => Self::Audio,
            _ => return None,
        })
    }

    /// The extension and mime type of this upload's thumb, medium and frame,
    /// given the served file's extension. Formats every browser shows keep
    /// their own; HEIF and AVIF stills and every video derivative are JPEG,
    /// because a thumbnail no browser can display is no thumbnail.
    ///
    /// This is the one source of truth for derivative storage keys: the
    /// media crate writes them with it and the service finds them with it.
    pub fn derivative_format(self, served_extension: &str) -> (&'static str, &'static str) {
        match (self, served_extension) {
            (Self::Image, "png") => ("png", "image/png"),
            (Self::Image, "gif") => ("gif", "image/gif"),
            (Self::Image, "webp") => ("webp", "image/webp"),
            _ => ("jpg", "image/jpeg"),
        }
    }
}

impl fmt::Display for MediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MediaKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "image" => Ok(Self::Image),
            "video" => Ok(Self::Video),
            "audio" => Ok(Self::Audio),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn extensions_know_their_kind() {
        use super::MediaKind;
        assert_eq!(MediaKind::of_extension("MP3"), Some(MediaKind::Audio));
        assert_eq!(MediaKind::of_extension("mov"), Some(MediaKind::Video));
        assert_eq!(MediaKind::of_extension("heic"), Some(MediaKind::Image));
        assert_eq!(MediaKind::of_extension("svg"), None);
    }

    use super::*;

    #[test]
    fn wire_strings_round_trip() {
        for kind in MediaKind::ALL {
            assert_eq!(kind.as_str().parse::<MediaKind>(), Ok(kind));
        }
    }

    #[test]
    fn unknown_kinds_are_rejected() {
        for input in ["", "document", "pdf", "Image", "IMAGE", "0", "1"] {
            assert!(input.parse::<MediaKind>().is_err(), "{input:?} parsed");
        }
    }

    #[test]
    fn only_video_needs_a_frame() {
        assert!(MediaKind::Video.needs_frame());
        assert!(!MediaKind::Image.needs_frame());
        assert!(!MediaKind::Audio.needs_frame());
    }

    #[test]
    fn derivatives_are_always_browser_viewable() {
        let image = MediaKind::Image;
        assert_eq!(image.derivative_format("png").0, "png");
        assert_eq!(image.derivative_format("webp").0, "webp");
        assert_eq!(image.derivative_format("gif").0, "gif");
        for served in ["jpg", "heic", "heif", "avif"] {
            assert_eq!(image.derivative_format(served).0, "jpg", "{served}");
        }
        assert_eq!(MediaKind::Video.derivative_format("webm").0, "jpg");
    }

    #[test]
    fn audio_has_no_visual_derivatives() {
        assert!(!MediaKind::Audio.has_visual_derivatives());
        assert!(MediaKind::Image.has_visual_derivatives());
        assert!(MediaKind::Video.has_visual_derivatives());
    }
}
