//! The `chain` bitmask: which derivatives an upload actually has.

use super::derivative::Derivative;

/// A set of [`Derivative`]s, stored as the legacy API's `chain` bitmask.
///
/// Which bits get set for a given upload is the derivative planner's decision
/// (see `haloid-media`), not this type's. This type only does the arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Chain(u8);

impl Chain {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn with(self, derivative: Derivative) -> Self {
        Self(self.0 | derivative.bit())
    }

    #[must_use]
    pub const fn without(self, derivative: Derivative) -> Self {
        Self(self.0 & !derivative.bit())
    }

    pub const fn contains(self, derivative: Derivative) -> bool {
        self.0 & derivative.bit() != 0
    }

    pub fn insert(&mut self, derivative: Derivative) {
        self.0 |= derivative.bit();
    }

    /// The derivatives present, in bit order (frame first).
    pub fn iter(self) -> impl Iterator<Item = Derivative> {
        Derivative::ALL
            .into_iter()
            .filter(move |d| self.contains(*d))
    }

    /// The chain as the legacy API reports it: upstream never keeps an
    /// original, so its bit is masked out there. v2 reports the real chain.
    #[must_use]
    pub const fn legacy(self) -> Self {
        self.without(Derivative::Original)
    }

    /// Human-readable derivative names, for the v2 API. The legacy API emits
    /// the raw bitmask instead; nothing in v2 should require a client to know
    /// legacy trivia.
    pub fn to_labels(self) -> Vec<&'static str> {
        self.iter().map(Derivative::legacy_key).collect()
    }
}

impl FromIterator<Derivative> for Chain {
    fn from_iter<I: IntoIterator<Item = Derivative>>(iter: I) -> Self {
        iter.into_iter().fold(Self::empty(), Self::with)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_contains_nothing() {
        let chain = Chain::empty();
        assert!(chain.is_empty());
        assert_eq!(chain.bits(), 0);
        for d in Derivative::ALL {
            assert!(!chain.contains(d));
        }
        assert!(chain.to_labels().is_empty());
    }

    #[test]
    fn known_combinations_have_known_bitmasks() {
        // The video from the legacy API's documentation: chain 21, with
        // medium_size 0. 21 = 16 frame + 4 image + 1 thumb, i.e. a video that
        // was small enough to skip the medium.
        let video: Chain = [Derivative::Frame, Derivative::Image, Derivative::Thumb]
            .into_iter()
            .collect();
        assert_eq!(video.bits(), 21);

        // A typical image: served file, medium, thumb.
        let image: Chain = [Derivative::Image, Derivative::Medium, Derivative::Thumb]
            .into_iter()
            .collect();
        assert_eq!(image.bits(), 7);

        // A resized upload keeps the untouched original as well.
        assert_eq!(image.with(Derivative::Original).bits(), 15);

        // Audio carries only the served file. The legacy API has no audio
        // value of its own, so this is ours: 8 (original) would null out
        // `image.url`.
        let audio: Chain = [Derivative::Image].into_iter().collect();
        assert_eq!(audio.bits(), 4);
    }

    #[test]
    fn every_subset_round_trips_through_bits() {
        for bits in 0u8..32 {
            let chain = Chain::from_bits(bits);
            let rebuilt: Chain = chain.iter().collect();
            assert_eq!(rebuilt.bits(), bits, "subset {bits} did not round-trip");
        }
    }

    #[test]
    fn with_and_without_are_inverses() {
        for d in Derivative::ALL {
            let chain = Chain::empty().with(d);
            assert!(chain.contains(d));
            assert!(!chain.without(d).contains(d));
            // Idempotent: adding twice is adding once.
            assert_eq!(chain.with(d), chain);
        }
    }

    #[test]
    fn iteration_is_in_bit_order() {
        let all: Chain = Derivative::ALL.into_iter().collect();
        assert_eq!(all.bits(), 31);
        assert_eq!(
            all.to_labels(),
            vec!["frame", "original", "image", "medium", "thumb"]
        );
    }

    #[test]
    fn the_legacy_chain_hides_a_retained_original() {
        let resized: Chain = [
            Derivative::Original,
            Derivative::Image,
            Derivative::Medium,
            Derivative::Thumb,
        ]
        .into_iter()
        .collect();
        assert_eq!(resized.bits(), 15);
        assert_eq!(resized.legacy().bits(), 7);
        assert_eq!(
            Chain::from_bits(21).legacy().bits(),
            21,
            "nothing else changes"
        );
    }

    #[test]
    fn insert_mutates_in_place() {
        let mut chain = Chain::empty();
        chain.insert(Derivative::Thumb);
        chain.insert(Derivative::Thumb);
        chain.insert(Derivative::Medium);
        assert_eq!(chain.bits(), 3);
    }
}
