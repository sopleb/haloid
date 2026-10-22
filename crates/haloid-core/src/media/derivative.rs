//! The individual files that make up one upload.

/// One stored file belonging to an upload.
///
/// `Image` is the file we serve; `Original` is a retained untouched upload,
/// which only exists when the served file differs from what was sent (a
/// `width` resize, or a watermark). For most uploads they are the same bytes
/// and only `Image` is present. When both exist they are separate objects:
/// the original carries an `.or` infix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Derivative {
    /// The retained, unmodified upload. Present only when the served file was
    /// altered. Upstream never keeps one, so the legacy API masks its bit out
    /// (see `Chain::legacy`).
    Original,
    /// The file we serve as the full-size view.
    Image,
    /// Longest edge 500px. Skipped when the source is already that small.
    Medium,
    /// 160x160, centre-cropped.
    Thumb,
    /// A still extracted from a video, from which its thumb and medium derive.
    Frame,
}

impl Derivative {
    /// This derivative's bit in the `chain` mask. These five values are
    /// fixed by the legacy wire contract — do not tidy them
    /// into sequential order.
    pub const fn bit(self) -> u8 {
        match self {
            Self::Frame => 16,
            Self::Original => 8,
            Self::Image => 4,
            Self::Medium => 2,
            Self::Thumb => 1,
        }
    }

    /// The filename infix, between the name and the extension:
    /// `photo.jpg`, `photo.th.jpg`, `photo.md.jpg`, `photo.fr.jpg`, and
    /// `photo.or.jpg` for a retained original.
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Image => "",
            Self::Original => ".or",
            Self::Medium => ".md",
            Self::Thumb => ".th",
            Self::Frame => ".fr",
        }
    }

    /// The key used for this derivative in the legacy response, where each one
    /// is its own sub-object.
    pub const fn legacy_key(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Image => "image",
            Self::Medium => "medium",
            Self::Thumb => "thumb",
            Self::Frame => "frame",
        }
    }

    pub const ALL: [Derivative; 5] = [
        Self::Frame,
        Self::Original,
        Self::Image,
        Self::Medium,
        Self::Thumb,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_match_the_legacy_values() {
        // Pinned: these are a wire contract, not an implementation detail.
        assert_eq!(Derivative::Frame.bit(), 16);
        assert_eq!(Derivative::Original.bit(), 8);
        assert_eq!(Derivative::Image.bit(), 4);
        assert_eq!(Derivative::Medium.bit(), 2);
        assert_eq!(Derivative::Thumb.bit(), 1);
    }

    #[test]
    fn bits_are_distinct_single_bits() {
        let mut seen = 0u8;
        for d in Derivative::ALL {
            assert_eq!(d.bit().count_ones(), 1, "{d:?} is not a single bit");
            assert_eq!(seen & d.bit(), 0, "{d:?} reuses a bit");
            seen |= d.bit();
        }
        assert_eq!(seen, 31);
    }

    #[test]
    fn a_retained_original_never_collides_with_the_served_file() {
        assert_eq!(Derivative::Image.suffix(), "");
        assert_eq!(Derivative::Original.suffix(), ".or");
        let mut suffixes: Vec<&str> = Derivative::ALL.iter().map(|d| d.suffix()).collect();
        suffixes.sort_unstable();
        suffixes.dedup();
        assert_eq!(
            suffixes.len(),
            Derivative::ALL.len(),
            "two derivatives share a key"
        );
    }

    #[test]
    fn suffixes_are_the_legacy_ones() {
        // Keeping .th / .md / .fr means an rsync from an existing install is
        // mechanical rather than a rename pass.
        assert_eq!(Derivative::Thumb.suffix(), ".th");
        assert_eq!(Derivative::Medium.suffix(), ".md");
        assert_eq!(Derivative::Frame.suffix(), ".fr");
    }
}
