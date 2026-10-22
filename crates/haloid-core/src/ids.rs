//! Typed row identifiers.
//!
//! The internal `i64`/`u64` row id is never exposed. [`IdEncoded`] is the
//! public-facing random string stored alongside it; see the `haloid-id` crate.

use std::fmt;

macro_rules! row_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
    };
}

row_id!(
    FileId,
    "Internal row id of a file. Never exposed to clients."
);
row_id!(AlbumId, "Internal row id of an album.");
row_id!(UserId, "Internal row id of a user.");
row_id!(StorageId, "Internal row id of a configured storage.");
row_id!(TagId, "Internal row id of a tag.");
row_id!(ApiKeyId, "Internal row id of an API key.");

/// Internal row id of a category. Categories are small and their ids are
/// public in the legacy API (`category_id` is sent as a plain integer, unlike
/// `album_id` which is sent encoded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CategoryId(pub u32);

/// A public identifier as it appears in URLs and API responses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdEncoded(pub String);

impl IdEncoded {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdEncoded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for IdEncoded {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for IdEncoded {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// A login session's id: a UUID, never the cookie token. The token is only
/// ever stored hashed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
