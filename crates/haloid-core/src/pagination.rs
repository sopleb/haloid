//! Keyset (cursor) pagination.
//!
//! Every list is paginated by cursor, never by offset: `LIMIT 100 OFFSET
//! 900000` on a million-row table is a scan, and offsets skip or repeat rows
//! when the list changes between pages. A cursor is the last row's sort key
//! plus its id as the tie-breaker.
//!
//! The text form is opaque to clients but deliberately not secret: tampering
//! with it can only start a page somewhere else in a list the caller is
//! already allowed to read.

use crate::DomainError;
use std::fmt;
use std::str::FromStr;

/// The default and largest page sizes.
pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 100;

/// A requested page size, defaulted and clamped into `1..=MAX_LIMIT`.
pub fn clamp_limit(requested: Option<u32>) -> u32 {
    requested.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// A list ordering that can be paginated. Each has a one-character code in
/// the cursor, so a cursor from one ordering cannot be replayed against
/// another.
pub trait SortOrder: Copy + Eq + fmt::Debug {
    fn code(self) -> char;
    fn from_code(code: char) -> Option<Self>;
}

/// Where the next page starts: after the row with this sort key and id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor<S> {
    pub sort: S,
    /// The sort column's value: unix seconds for dates, the count for
    /// counters, the position for manual order.
    pub key: i64,
    pub id: u64,
}

impl<S: SortOrder> Cursor<S> {
    pub const fn new(sort: S, key: i64, id: u64) -> Self {
        Self { sort, key, id }
    }

    /// Parse a client-supplied cursor, which must belong to `sort`.
    pub fn parse(text: &str, sort: S) -> Result<Self, DomainError> {
        let invalid = || DomainError::invalid_param("cursor", "not a cursor for this list");
        let cursor: Self = text.parse().map_err(|()| invalid())?;
        if cursor.sort != sort {
            return Err(invalid());
        }
        Ok(cursor)
    }
}

impl<S: SortOrder> fmt::Display for Cursor<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.sort.code(), self.key, self.id)
    }
}

impl<S: SortOrder> FromStr for Cursor<S> {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split('.');
        let (Some(code), Some(key), Some(id), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(());
        };
        let mut chars = code.chars();
        let (Some(code), None) = (chars.next(), chars.next()) else {
            return Err(());
        };
        Ok(Self {
            sort: S::from_code(code).ok_or(())?,
            key: key.parse().map_err(|_| ())?,
            id: id.parse().map_err(|_| ())?,
        })
    }
}

/// One page of a list. `next` is `None` on the last page.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T, S> {
    pub items: Vec<T>,
    pub next: Option<Cursor<S>>,
}

/// How a file list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileSort {
    Newest,
    Oldest,
    MostViewed,
    MostLiked,
    /// The owner's manual arrangement. Only meaningful within one album.
    AlbumOrder,
}

impl SortOrder for FileSort {
    fn code(self) -> char {
        match self {
            Self::Newest => 'n',
            Self::Oldest => 'o',
            Self::MostViewed => 'v',
            Self::MostLiked => 'l',
            Self::AlbumOrder => 'a',
        }
    }

    fn from_code(code: char) -> Option<Self> {
        Some(match code {
            'n' => Self::Newest,
            'o' => Self::Oldest,
            'v' => Self::MostViewed,
            'l' => Self::MostLiked,
            'a' => Self::AlbumOrder,
            _ => return None,
        })
    }
}

/// How an album list is ordered. Albums are few per user; newest first is
/// the only order milestone one needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlbumSort {
    Newest,
}

impl SortOrder for AlbumSort {
    fn code(self) -> char {
        'n'
    }

    fn from_code(code: char) -> Option<Self> {
        (code == 'n').then_some(Self::Newest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [FileSort; 5] = [
        FileSort::Newest,
        FileSort::Oldest,
        FileSort::MostViewed,
        FileSort::MostLiked,
        FileSort::AlbumOrder,
    ];

    #[test]
    fn cursors_round_trip_through_text() {
        for sort in ALL {
            for (key, id) in [(0, 1), (1_729_512_345, 42), (-5, u64::MAX), (i64::MAX, 0)] {
                let cursor = Cursor::new(sort, key, id);
                let text = cursor.to_string();
                assert_eq!(Cursor::parse(&text, sort).unwrap(), cursor, "{text}");
            }
        }
    }

    #[test]
    fn the_text_form_is_url_safe() {
        let text = Cursor::new(FileSort::Newest, -1_729_512_345, 42).to_string();
        assert!(
            text.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'),
            "{text}"
        );
    }

    #[test]
    fn a_cursor_from_another_ordering_is_rejected() {
        let views = Cursor::new(FileSort::MostViewed, 10, 3).to_string();
        let error = Cursor::parse(&views, FileSort::Newest).unwrap_err();
        assert!(matches!(error, DomainError::InvalidParam { ref field, .. } if field == "cursor"));
    }

    #[test]
    fn malformed_cursors_are_invalid_params() {
        for text in [
            "", "n", "n.1", "n.1.2.3", "x.1.2", "nn.1.2", "n.a.2", "n.1.-2", "n.1.b", "n..2",
        ] {
            assert!(
                Cursor::parse(text, FileSort::Newest).is_err(),
                "{text:?} was accepted"
            );
        }
    }

    #[test]
    fn sort_codes_are_distinct() {
        let mut codes: Vec<char> = ALL.iter().map(|s| s.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), ALL.len());
        for sort in ALL {
            assert_eq!(FileSort::from_code(sort.code()), Some(sort));
        }
    }

    #[test]
    fn limits_default_and_clamp() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(10)), 10);
        assert_eq!(clamp_limit(Some(10_000)), MAX_LIMIT);
    }
}
