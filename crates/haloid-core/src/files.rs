//! Stored files and albums, as the domain sees them.
//!
//! Deletion is retained, not destroyed: a deleted file keeps its row and its
//! bytes, with who deleted it and why, until an administrator purges it. That
//! is what makes "what happened to this file" answerable after the fact.

use crate::ids::{AlbumId, CategoryId, FileId, IdEncoded, StorageId, UserId};
use crate::media::{Chain, MediaKind};
use crate::storage::StoragePathMode;
use chrono::{DateTime, Utc};
use std::str::FromStr;

/// Where a file is in its upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileState {
    /// The row exists but the bytes have not reached storage. Nothing lists
    /// or serves a pending file.
    Pending,
    Ready,
}

/// Why a file was deleted. Recorded on the row for audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeleteReason {
    /// Its owner deleted it.
    Owner,
    /// A moderator removed it.
    Moderation,
    /// An administrator removed it, outside moderation.
    Admin,
    /// Someone presented the upload's one-shot delete token — a guest,
    /// usually.
    DeleteToken,
    /// Its requested expiry passed.
    Expired,
    /// It went with an album that was deleted together with its contents.
    Album,
}

macro_rules! wire_enum {
    ($ty:ty { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $ty {
            /// The wire and database representation.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
        }

        impl FromStr for $ty {
            type Err = ();

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok(Self::$variant),)+
                    _ => Err(()),
                }
            }
        }
    };
}

wire_enum!(FileState { Pending => "pending", Ready => "ready" });
wire_enum!(DeleteReason {
    Owner => "owner",
    Moderation => "moderation",
    Admin => "admin",
    DeleteToken => "delete_token",
    Expired => "expired",
    Album => "album",
});
wire_enum!(AlbumPrivacy {
    Public => "public",
    Unlisted => "unlisted",
    Private => "private",
    Password => "password",
});

/// When, by whom and why a file was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deletion {
    pub at: DateTime<Utc>,
    /// `None` when no account did it: a delete token, or expiry.
    pub by: Option<UserId>,
    pub reason: DeleteReason,
}

/// One upload. Named to keep it apart from `std::fs::File`.
///
/// EXIF, the uploader's IP and the delete token are deliberately not here:
/// the first is bulky and the other two are sensitive. Each has its own
/// lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFile {
    pub id: FileId,
    pub id_encoded: IdEncoded,
    pub kind: MediaKind,
    pub state: FileState,
    /// Stored basename without extension, including its random suffix.
    pub name: String,
    pub extension: String,
    /// The retained original's extension, when there is one (chain bit 8).
    /// It can differ from `extension`: a resized HEIC is served as JPEG.
    pub original_extension: Option<String>,
    pub mime: String,
    pub original_filename: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub duration_ms: Option<u32>,
    pub is_animated: bool,
    pub frame_count: Option<u32>,
    pub chain: Chain,
    pub thumb_size: Option<u64>,
    pub medium_size: Option<u64>,
    pub frame_size: Option<u64>,
    pub checksum: String,
    pub source_checksum: Option<String>,
    pub sha256: Option<String>,
    pub storage_id: Option<StorageId>,
    pub storage_mode: StoragePathMode,
    pub storage_path: String,
    /// `None` is a guest upload.
    pub user_id: Option<UserId>,
    pub album_id: Option<AlbumId>,
    pub album_position: Option<u32>,
    pub category_id: Option<CategoryId>,
    pub nsfw: bool,
    pub is_approved: bool,
    pub is_360: bool,
    pub view_count: u64,
    pub like_count: u32,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deletion: Option<Deletion>,
}

impl MediaFile {
    /// Every stored byte of this upload: the file and each derivative. This
    /// is what a user's `disk_used` counts.
    pub fn stored_bytes(&self) -> u64 {
        [self.thumb_size, self.medium_size, self.frame_size]
            .into_iter()
            .flatten()
            .fold(self.size, u64::saturating_add)
    }

    pub const fn is_deleted(&self) -> bool {
        self.deletion.is_some()
    }

    /// Whether this file may be served or listed at all, to anyone, at `now`.
    /// Moderation and privacy are further checks on top of this.
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        matches!(self.state, FileState::Ready)
            && self.deletion.is_none()
            && self.expires_at.is_none_or(|at| at > now)
    }
}

/// Who may see an album.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlbumPrivacy {
    /// Listed and viewable by anyone.
    Public,
    /// Viewable by anyone with the link, never listed.
    Unlisted,
    /// Its owner and moderators only.
    Private,
    /// Anyone who knows the album's password.
    Password,
}

impl AlbumPrivacy {
    /// Whether this album's files may appear in public listings. Only public
    /// albums: files inherit their album's privacy.
    pub const fn is_listed(self) -> bool {
        matches!(self, Self::Public)
    }
}

/// The deepest an album tree may nest. Deep enough for any real filing
/// scheme, shallow enough that walking it is never a concern.
pub const MAX_ALBUM_DEPTH: u32 = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct Album {
    pub id: AlbumId,
    pub id_encoded: IdEncoded,
    pub user_id: Option<UserId>,
    pub parent_id: Option<AlbumId>,
    pub name: String,
    pub description: Option<String>,
    pub privacy: AlbumPrivacy,
    /// Whether a password is set. The hash itself has its own lookup.
    pub has_password: bool,
    pub cover_file_id: Option<FileId>,
    /// Live files directly in this album, not in its sub-albums.
    pub file_count: u32,
    pub view_count: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    fn file() -> MediaFile {
        MediaFile {
            id: FileId(1),
            id_encoded: "abcdefghij".into(),
            kind: MediaKind::Image,
            state: FileState::Ready,
            name: "cat-ab12".into(),
            extension: "jpg".into(),
            original_extension: None,
            mime: "image/jpeg".into(),
            original_filename: None,
            title: None,
            description: None,
            size: 1000,
            width: 10,
            height: 10,
            duration_ms: None,
            is_animated: false,
            frame_count: None,
            chain: Chain::from_bits(5),
            thumb_size: Some(100),
            medium_size: None,
            frame_size: None,
            checksum: "0".repeat(32),
            source_checksum: None,
            sha256: None,
            storage_id: None,
            storage_mode: StoragePathMode::DateFolder,
            storage_path: "2026/10/21/cat-ab12.jpg".into(),
            user_id: None,
            album_id: None,
            album_position: None,
            category_id: None,
            nsfw: false,
            is_approved: true,
            is_360: false,
            view_count: 0,
            like_count: 0,
            expires_at: None,
            created_at: at(0),
            updated_at: at(0),
            deletion: None,
        }
    }

    #[test]
    fn wire_names_round_trip() {
        for state in [FileState::Pending, FileState::Ready] {
            assert_eq!(state.as_str().parse::<FileState>(), Ok(state));
        }
        for reason in [
            DeleteReason::Owner,
            DeleteReason::Moderation,
            DeleteReason::Admin,
            DeleteReason::DeleteToken,
            DeleteReason::Expired,
            DeleteReason::Album,
        ] {
            assert_eq!(reason.as_str().parse::<DeleteReason>(), Ok(reason));
        }
        for privacy in [
            AlbumPrivacy::Public,
            AlbumPrivacy::Unlisted,
            AlbumPrivacy::Private,
            AlbumPrivacy::Password,
        ] {
            assert_eq!(privacy.as_str().parse::<AlbumPrivacy>(), Ok(privacy));
        }
        assert!("deleted".parse::<FileState>().is_err());
    }

    #[test]
    fn stored_bytes_counts_every_derivative() {
        let mut f = file();
        assert_eq!(f.stored_bytes(), 1100);
        f.medium_size = Some(10);
        f.frame_size = Some(1);
        assert_eq!(f.stored_bytes(), 1111);
        f.size = u64::MAX;
        assert_eq!(f.stored_bytes(), u64::MAX, "saturates");
    }

    #[test]
    fn only_ready_undeleted_unexpired_files_are_live() {
        let now = at(100);
        assert!(file().is_live(now));

        let mut pending = file();
        pending.state = FileState::Pending;
        assert!(!pending.is_live(now));

        let mut deleted = file();
        deleted.deletion = Some(Deletion {
            at: at(50),
            by: None,
            reason: DeleteReason::DeleteToken,
        });
        assert!(!deleted.is_live(now));
        assert!(deleted.is_deleted());

        let mut expiring = file();
        expiring.expires_at = Some(at(100));
        assert!(!expiring.is_live(now), "expired at its instant");
        assert!(expiring.is_live(at(99)));
    }

    #[test]
    fn only_public_albums_are_listed() {
        assert!(AlbumPrivacy::Public.is_listed());
        for p in [
            AlbumPrivacy::Unlisted,
            AlbumPrivacy::Private,
            AlbumPrivacy::Password,
        ] {
            assert!(!p.is_listed(), "{p:?}");
        }
    }
}
