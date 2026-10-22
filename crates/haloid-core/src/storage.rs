//! The storage abstraction.
//!
//! Implemented by `haloid-storage` for the local filesystem and for
//! S3-compatible services. The trait lives here so that nothing in the domain
//! or service layer knows which one it is talking to.

use crate::StorageError;
use crate::ids::StorageId;
use std::path::PathBuf;
use std::str::FromStr;

/// How a stored object's key is laid out under its storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePathMode {
    /// `prefix/YYYY/MM/DD/name.ext` — the default, and the legacy layout.
    DateFolder,
    /// `prefix/name.ext`
    Direct,
    /// A configured template with `{yyyy}`, `{user_id}`, `{id_encoded}` and
    /// friends substituted.
    Template,
}

impl StoragePathMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DateFolder => "datefolder",
            Self::Direct => "direct",
            Self::Template => "path",
        }
    }
}

impl FromStr for StoragePathMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "datefolder" => Ok(Self::DateFolder),
            "direct" => Ok(Self::Direct),
            "path" => Ok(Self::Template),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    Local,
    S3,
}

impl StorageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::S3 => "s3",
        }
    }
}

impl FromStr for StorageKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "local" => Ok(Self::Local),
            "s3" => Ok(Self::S3),
            _ => Err(()),
        }
    }
}

/// A configured storage, as administered. Credentials are never part of it:
/// they come from the environment, keyed by `name`, so that a database dump
/// is not a credential leak.
#[derive(Debug, Clone, PartialEq)]
pub struct StorageRecord {
    pub id: StorageId,
    /// Unique, and the key for this storage's credentials in the environment.
    pub name: String,
    pub kind: StorageKind,
    /// Kind-specific settings as JSON text: bucket, endpoint, region, base
    /// path. Interpreted by `haloid-storage`, which owns the shape.
    pub config: String,
    /// Public URL the stored keys are served under.
    pub url_prefix: String,
    pub path_mode: StoragePathMode,
    pub path_template: Option<String>,
    /// `None` is unlimited.
    pub capacity_bytes: Option<u64>,
    /// Bytes physically held, deleted-but-unpurged files included.
    pub used_bytes: u64,
    pub file_count: u64,
    pub is_active: bool,
    /// Higher is preferred when choosing where an upload goes.
    pub priority: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Retired: takes no new uploads and keeps serving what it holds.
    pub retired_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl StorageRecord {
    /// Whether an upload of `size` bytes fits.
    pub fn has_room_for(&self, size: u64) -> bool {
        self.capacity_bytes
            .is_none_or(|cap| self.used_bytes.saturating_add(size) <= cap)
    }
}

/// What to write. A path variant exists so that local storage can rename a
/// temp file into place and S3 can stream it, rather than forcing every upload
/// through memory — a video must never be buffered whole.
#[derive(Debug)]
pub enum PutBody {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    pub size: u64,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageUsage {
    pub bytes: u64,
    pub objects: u64,
}

#[async_trait::async_trait]
pub trait Storage: Send + Sync + 'static {
    fn id(&self) -> StorageId;
    fn kind(&self) -> StorageKind;

    /// The public URL a stored key is served from.
    fn url_for(&self, key: &str) -> String;

    async fn put(
        &self,
        key: &str,
        body: PutBody,
        content_type: &str,
    ) -> Result<ObjectMeta, StorageError>;

    async fn head(&self, key: &str) -> Result<ObjectMeta, StorageError>;

    /// Remove an object. Deleting one that is already gone succeeds, so a
    /// retried purge does not fail on the part that worked last time.
    async fn delete(&self, key: &str) -> Result<(), StorageError>;

    /// Deleting many keys at once. Returns the keys that failed rather than
    /// aborting, so a partial purge can be retried for just the remainder.
    async fn delete_many(&self, keys: &[String]) -> Result<Vec<String>, StorageError>;

    /// Potentially expensive — a full listing on object stores. Jobs only,
    /// never a request path.
    async fn usage(&self) -> Result<StorageUsage, StorageError>;

    /// Move an object out of public reach, for a deleted file: it stops being
    /// served at [`Storage::url_for`] but is kept, privately, until purged.
    ///
    /// Idempotent, as are the two below, so an interrupted batch can simply
    /// be run again: quarantining an object that is already quarantined
    /// succeeds. Only an object found in neither place is `NotFound`.
    async fn quarantine(&self, key: &str) -> Result<(), StorageError>;

    /// Undo [`Storage::quarantine`], for a restored file.
    async fn release(&self, key: &str) -> Result<(), StorageError>;

    /// Permanently remove a quarantined object, for a purge. Succeeds if it
    /// is already gone.
    async fn delete_quarantined(&self, key: &str) -> Result<(), StorageError>;

    /// The public URL a quarantined object would be reachable at if the
    /// storage were misconfigured to serve it, for the storage test to
    /// probe. `None` where quarantine cannot be reached by URL at all, as
    /// for local storage, whose quarantine is outside every served root.
    fn quarantined_public_url(&self, _key: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_mode_wire_names_match_the_database_enum() {
        assert_eq!(StoragePathMode::DateFolder.as_str(), "datefolder");
        assert_eq!(StoragePathMode::Direct.as_str(), "direct");
        // The database calls the template mode "path", the legacy name.
        assert_eq!(StoragePathMode::Template.as_str(), "path");
    }

    #[test]
    fn kinds_round_trip() {
        for kind in [StorageKind::Local, StorageKind::S3] {
            assert_eq!(kind.as_str().parse::<StorageKind>(), Ok(kind));
        }
    }

    #[test]
    fn room_is_measured_against_capacity() {
        let mut storage = StorageRecord {
            id: StorageId(1),
            name: "main".into(),
            kind: StorageKind::Local,
            config: "{}".into(),
            url_prefix: "http://localhost/files".into(),
            path_mode: StoragePathMode::DateFolder,
            path_template: None,
            capacity_bytes: None,
            used_bytes: 900,
            file_count: 1,
            is_active: true,
            priority: 0,
            created_at: chrono::DateTime::UNIX_EPOCH,
            updated_at: chrono::DateTime::UNIX_EPOCH,
            retired_at: None,
        };
        assert!(storage.has_room_for(u64::MAX), "unlimited");
        storage.capacity_bytes = Some(1000);
        assert!(storage.has_room_for(100));
        assert!(!storage.has_room_for(101));
    }

    #[test]
    fn path_modes_parse_from_their_wire_names() {
        for mode in [
            StoragePathMode::DateFolder,
            StoragePathMode::Direct,
            StoragePathMode::Template,
        ] {
            assert_eq!(mode.as_str().parse::<StoragePathMode>(), Ok(mode));
        }
        assert!("template".parse::<StoragePathMode>().is_err());
    }
}
