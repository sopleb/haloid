//! The media processing abstraction.
//!
//! Implemented by `haloid-media` over libvips and ffmpeg. It is a trait
//! because the libvips bindings are a thin FFI wrapper with a process-global
//! error buffer: if they prove unreliable, shelling out to the `vips` and
//! `vipsthumbnail` CLIs is a second implementation rather than a rewrite.

use crate::MediaError;
use crate::media::{Chain, Derivative, MediaKind, ProbeResult};
use crate::settings::{DerivativeSettings, MediumDimension};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Which derivatives to make, and at what sizes. Built from the settings
/// with [`DerivativePlan::new`]; [`DerivativePlan::expected_chain`] is the
/// pure statement of what an upload ends up with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivativePlan {
    pub kind: MediaKind,
    /// Square edge of the thumbnail, centre-cropped.
    pub thumb_edge: u32,
    /// The medium derivative's size along `medium_dimension` (fitting inside
    /// a square of this edge for `Longest`).
    pub medium_edge: u32,
    /// Skip the medium entirely when the source measures at or under this
    /// along `medium_dimension`. The reason `medium_size` is nullable and
    /// `medium{}` serialises as all-nulls.
    pub medium_threshold: u32,
    pub medium_dimension: MediumDimension,
    /// Keep animation in derivatives instead of using the first frame.
    pub animated_derivatives: bool,
}

impl DerivativePlan {
    pub fn new(kind: MediaKind, settings: &DerivativeSettings) -> Self {
        Self {
            kind,
            thumb_edge: settings.thumb_edge,
            medium_edge: settings.medium_edge,
            medium_threshold: settings.medium_threshold,
            medium_dimension: settings.medium_fixed_dimension,
            animated_derivatives: settings.animated_derivatives,
        }
    }

    /// Whether a source of this display size gets a medium. Measured on the
    /// image for stills and animations, on the extracted frame for video;
    /// animated images follow the same size rule as stills.
    pub fn wants_medium(&self, width: u32, height: u32) -> bool {
        self.kind.has_visual_derivatives()
            && self.medium_dimension.measure(width, height) > self.medium_threshold
    }

    /// Everything an upload of this probe will carry. `altered` is whether
    /// the served file differs from what was sent (a resize, a watermark),
    /// which keeps the untouched original as well.
    pub fn expected_chain(&self, probe: &ProbeResult, altered: bool) -> Chain {
        let mut chain = Chain::empty().with(Derivative::Image);
        if altered {
            chain.insert(Derivative::Original);
        }
        if self.kind.has_visual_derivatives() {
            chain.insert(Derivative::Thumb);
            if self.wants_medium(probe.width, probe.height) {
                chain.insert(Derivative::Medium);
            }
        }
        if self.kind.needs_frame() {
            chain.insert(Derivative::Frame);
        }
        chain
    }
}

/// One produced file on disk, before it is handed to a storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivativeFile {
    pub path: PathBuf,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub mime: String,
    pub extension: String,
}

/// Everything a plan produced, plus the resulting [`Chain`].
#[derive(Debug, Clone, Default)]
pub struct DerivativeSet {
    pub files: BTreeMap<&'static str, DerivativeFile>,
}

impl DerivativeSet {
    pub fn get(&self, derivative: Derivative) -> Option<&DerivativeFile> {
        self.files.get(derivative.legacy_key())
    }

    pub fn insert(&mut self, derivative: Derivative, file: DerivativeFile) {
        self.files.insert(derivative.legacy_key(), file);
    }

    /// The bitmask for what was actually produced.
    pub fn chain(&self) -> Chain {
        Derivative::ALL
            .into_iter()
            .filter(|d| self.files.contains_key(d.legacy_key()))
            .collect()
    }
}

/// Where a watermark sits on the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatermarkPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

impl WatermarkPosition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
            Self::Center => "center",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "top-left" => Self::TopLeft,
            "top-right" => Self::TopRight,
            "bottom-left" => Self::BottomLeft,
            "bottom-right" => Self::BottomRight,
            "center" => Self::Center,
            _ => return None,
        })
    }

    /// The mark's top-left corner, for a mark of `mark` size on an image
    /// of `image` size, `margin` pixels from the edges it is anchored to.
    /// Never negative: a mark too big for its margin sits at the edge.
    pub fn place(self, image: (u32, u32), mark: (u32, u32), margin: u32) -> (u32, u32) {
        let room = |total: u32, size: u32| total.saturating_sub(size);
        let far = |total: u32, size: u32| room(total, size).saturating_sub(margin);
        let near = |total: u32, size: u32| margin.min(room(total, size));
        let (w, h) = image;
        let (mw, mh) = mark;
        match self {
            Self::TopLeft => (near(w, mw), near(h, mh)),
            Self::TopRight => (far(w, mw), near(h, mh)),
            Self::BottomLeft => (near(w, mw), far(h, mh)),
            Self::BottomRight => (far(w, mw), far(h, mh)),
            Self::Center => (room(w, mw) / 2, room(h, mh) / 2),
        }
    }
}

/// How to stamp a watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watermark {
    /// The operator's mark: an image, ideally a PNG with transparency.
    pub mark: PathBuf,
    pub position: WatermarkPosition,
    /// The mark's width as a percentage of the image's.
    pub width_percent: u8,
    pub opacity_percent: u8,
    pub margin: u32,
    /// Images smaller than this on either side are left alone.
    pub min_width: u32,
    pub min_height: u32,
}

#[async_trait::async_trait]
pub trait MediaProcessor: Send + Sync + 'static {
    /// Read dimensions, duration and animation off the bytes. Must reject
    /// decompression bombs before any full decode.
    async fn probe(&self, path: &Path) -> Result<ProbeResult, MediaError>;

    async fn derive(
        &self,
        path: &Path,
        probe: &ProbeResult,
        plan: &DerivativePlan,
    ) -> Result<DerivativeSet, MediaError>;

    /// Resize the file that will be served, for the legacy `width` parameter.
    /// Never enlarges. Returns the new file; the input is left as the
    /// retained original.
    async fn resize(&self, path: &Path, max_width: u32) -> Result<PathBuf, MediaError>;

    /// Stamp a watermark on the file that will be served. Images only, and
    /// not animations; anything else, or an image under the minimum size,
    /// comes back as the same path, untouched. Otherwise a new file, and
    /// the input is left as the retained original.
    async fn watermark(&self, path: &Path, watermark: &Watermark) -> Result<PathBuf, MediaError>;

    /// Remove identifying metadata (EXIF, XMP, comments, GPS) from the file
    /// that will be served, keeping only what display needs — its
    /// orientation and colour profile. In place. Run at upload, never per
    /// request.
    async fn strip_metadata(&self, path: &Path, probe: &ProbeResult) -> Result<(), MediaError>;

    /// The file's EXIF as a JSON object of tag name to display value, with
    /// MakerNote and any value over 4 KiB dropped. `None` when there is none.
    async fn read_exif(&self, path: &Path) -> Result<Option<String>, MediaError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watermark_is_placed_from_its_corner_and_never_off_the_image() {
        let (image, mark) = ((1000, 800), (200, 100));
        assert_eq!(WatermarkPosition::TopLeft.place(image, mark, 20), (20, 20));
        assert_eq!(
            WatermarkPosition::TopRight.place(image, mark, 20),
            (780, 20)
        );
        assert_eq!(
            WatermarkPosition::BottomLeft.place(image, mark, 20),
            (20, 680)
        );
        assert_eq!(
            WatermarkPosition::BottomRight.place(image, mark, 20),
            (780, 680)
        );
        assert_eq!(WatermarkPosition::Center.place(image, mark, 20), (400, 350));
        // A mark wider than the image, or a margin larger than the room.
        assert_eq!(
            WatermarkPosition::BottomRight.place((100, 100), (150, 50), 20),
            (0, 30)
        );
        assert_eq!(
            WatermarkPosition::TopLeft.place((100, 100), (90, 90), 20),
            (10, 10)
        );
        for p in [
            "top-left",
            "top-right",
            "bottom-left",
            "bottom-right",
            "center",
        ] {
            assert_eq!(WatermarkPosition::parse(p).unwrap().as_str(), p);
        }
        assert_eq!(WatermarkPosition::parse("middle"), None);
    }

    fn file() -> DerivativeFile {
        DerivativeFile {
            path: PathBuf::from("/tmp/x.jpg"),
            size: 1,
            width: 1,
            height: 1,
            mime: "image/jpeg".into(),
            extension: "jpg".into(),
        }
    }

    fn probe(kind: MediaKind, width: u32, height: u32, animated: bool) -> ProbeResult {
        ProbeResult {
            kind,
            mime: String::new(),
            extension: String::new(),
            width,
            height,
            duration_ms: None,
            is_animated: animated,
            frame_count: None,
        }
    }

    #[test]
    fn the_chain_for_every_kind_of_upload() {
        // The table the legacy wire depends on. Defaults: thumb 160, medium
        // 500, threshold 500 on width.
        let defaults = crate::settings::Settings::default().derivatives;
        let plan = |kind| DerivativePlan::new(kind, &defaults);
        use MediaKind::{Audio, Image, Video};
        for (what, kind, w, h, animated, altered, bits) in [
            ("large photo", Image, 1920, 1080, false, false, 7),
            ("small photo, no medium", Image, 400, 300, false, false, 5),
            ("exactly at the threshold", Image, 500, 300, false, false, 5),
            ("one over the threshold", Image, 501, 300, false, false, 7),
            (
                "tall and narrow, measured on width",
                Image,
                400,
                2000,
                false,
                false,
                5,
            ),
            (
                "small animation: size alone decides",
                Image,
                300,
                300,
                true,
                false,
                5,
            ),
            ("large animation", Image, 800, 600, true, false, 7),
            ("resized: original kept", Image, 1920, 1080, false, true, 15),
            ("video, small frame", Video, 320, 240, false, false, 21),
            ("video, large frame", Video, 1920, 1080, false, false, 23),
            ("audio carries only itself", Audio, 0, 0, false, false, 4),
        ] {
            let chain = plan(kind).expected_chain(&probe(kind, w, h, animated), altered);
            assert_eq!(chain.bits(), bits, "{what}");
        }
    }

    #[test]
    fn the_measured_side_is_configurable() {
        let mut settings = crate::settings::Settings::default().derivatives;
        let tall = probe(MediaKind::Image, 400, 2000, false);
        for (dimension, gets_medium) in [
            (MediumDimension::Width, false),
            (MediumDimension::Height, true),
            (MediumDimension::Longest, true),
        ] {
            settings.medium_fixed_dimension = dimension;
            let plan = DerivativePlan::new(MediaKind::Image, &settings);
            assert_eq!(
                plan.expected_chain(&tall, false)
                    .contains(Derivative::Medium),
                gets_medium,
                "{dimension:?}"
            );
        }
    }

    #[test]
    fn an_empty_set_has_an_empty_chain() {
        assert!(DerivativeSet::default().chain().is_empty());
    }

    #[test]
    fn the_chain_reflects_what_was_produced() {
        let mut set = DerivativeSet::default();
        set.insert(Derivative::Image, file());
        set.insert(Derivative::Thumb, file());
        // 4 + 1, no medium: the small-image case.
        assert_eq!(set.chain().bits(), 5);
        assert!(set.get(Derivative::Image).is_some());
        assert!(set.get(Derivative::Medium).is_none());
    }

    #[test]
    fn a_video_set_includes_the_frame_bit() {
        let mut set = DerivativeSet::default();
        for d in [Derivative::Frame, Derivative::Image, Derivative::Thumb] {
            set.insert(d, file());
        }
        // The chain 21 from the legacy API's documented video example.
        assert_eq!(set.chain().bits(), 21);
    }
}
