//! Storage on the local filesystem.
//!
//! Objects live under a root that the server serves statically at the
//! storage's URL prefix. Quarantined objects live under a second directory
//! that is never served, and which must not overlap the root — enforced at
//! construction, because a quarantine inside the served tree would quietly
//! keep serving every deleted file.
//!
//! Writes are atomic: bytes land in a temporary file beside their final name
//! and are renamed into place, so a reader never sees half an object.

use crate::keys::{encode_key, validate_key};
use async_trait::async_trait;
use haloid_core::StorageError;
use haloid_core::ids::StorageId;
use haloid_core::storage::{ObjectMeta, PutBody, Storage, StorageKind, StorageUsage};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct LocalStorage {
    id: StorageId,
    url_prefix: String,
    root: PathBuf,
    quarantine: PathBuf,
}

impl LocalStorage {
    /// Directories are created on first write, not here.
    pub fn new(
        id: StorageId,
        url_prefix: &str,
        root: &Path,
        quarantine: &Path,
    ) -> Result<Self, StorageError> {
        let root = std::path::absolute(root)?;
        let quarantine = std::path::absolute(quarantine)?;
        if quarantine.starts_with(&root) || root.starts_with(&quarantine) {
            return Err(StorageError::Misconfigured(format!(
                "quarantine {} overlaps the served directory {}",
                quarantine.display(),
                root.display()
            )));
        }
        Ok(Self {
            id,
            url_prefix: url_prefix.trim_end_matches('/').to_string(),
            root,
            quarantine,
        })
    }

    /// The served directory: what the server mounts at the URL prefix.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn served(&self, key: &str) -> Result<PathBuf, StorageError> {
        validate_key(key)?;
        Ok(self.root.join(key))
    }

    fn quarantined(&self, key: &str) -> Result<PathBuf, StorageError> {
        validate_key(key)?;
        Ok(self.quarantine.join(key))
    }
}

/// A sibling of `path` to write into before renaming over it. Unique within
/// the process, and dot-prefixed so it is not mistaken for an object.
fn temp_beside(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

async fn ensure_parent(path: &Path) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    Ok(())
}

/// Move a file, falling back to copy-then-remove when the two paths are on
/// different filesystems (a temp directory on tmpfs, say). The copy lands in
/// a temporary sibling first so the destination is never seen half-written.
async fn move_file(from: &Path, to: &Path) -> Result<(), StorageError> {
    ensure_parent(to).await?;
    if tokio::fs::rename(from, to).await.is_ok() {
        return Ok(());
    }
    let temp = temp_beside(to);
    tokio::fs::copy(from, &temp).await?;
    if let Err(e) = tokio::fs::rename(&temp, to).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(e.into());
    }
    tokio::fs::remove_file(from).await?;
    Ok(())
}

async fn exists(path: &Path) -> Result<bool, StorageError> {
    Ok(tokio::fs::try_exists(path).await?)
}

/// Remove a file, treating "already gone" as done.
async fn remove(path: &Path) -> Result<(), StorageError> {
    match tokio::fs::remove_file(path).await {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[async_trait]
impl Storage for LocalStorage {
    fn id(&self) -> StorageId {
        self.id
    }

    fn kind(&self) -> StorageKind {
        StorageKind::Local
    }

    fn url_for(&self, key: &str) -> String {
        format!("{}/{}", self.url_prefix, encode_key(key))
    }

    async fn put(
        &self,
        key: &str,
        body: PutBody,
        content_type: &str,
    ) -> Result<ObjectMeta, StorageError> {
        let path = self.served(key)?;
        ensure_parent(&path).await?;
        match body {
            PutBody::Path(source) => move_file(&source, &path).await?,
            PutBody::Bytes(bytes) => {
                let temp = temp_beside(&path);
                tokio::fs::write(&temp, bytes).await?;
                if let Err(e) = tokio::fs::rename(&temp, &path).await {
                    let _ = tokio::fs::remove_file(&temp).await;
                    return Err(e.into());
                }
            }
        }
        let size = tokio::fs::metadata(&path).await?.len();
        Ok(ObjectMeta {
            size,
            content_type: Some(content_type.to_string()),
        })
    }

    async fn head(&self, key: &str) -> Result<ObjectMeta, StorageError> {
        let path = self.served(key)?;
        match tokio::fs::metadata(&path).await {
            Ok(meta) => Ok(ObjectMeta {
                size: meta.len(),
                content_type: None,
            }),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                Err(StorageError::NotFound(key.to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        remove(&self.served(key)?).await
    }

    async fn delete_many(&self, keys: &[String]) -> Result<Vec<String>, StorageError> {
        let mut failed = Vec::new();
        for key in keys {
            if let Err(error) = self.delete(key).await {
                tracing::warn!(key, %error, "could not delete object");
                failed.push(key.clone());
            }
        }
        Ok(failed)
    }

    async fn usage(&self) -> Result<StorageUsage, StorageError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || walk(&root))
            .await
            .map_err(|e| StorageError::Io(std::io::Error::other(e)))?
    }

    async fn quarantine(&self, key: &str) -> Result<(), StorageError> {
        let (from, to) = (self.served(key)?, self.quarantined(key)?);
        if exists(&from).await? {
            return move_file(&from, &to).await;
        }
        if exists(&to).await? {
            return Ok(());
        }
        Err(StorageError::NotFound(key.to_string()))
    }

    async fn release(&self, key: &str) -> Result<(), StorageError> {
        let (from, to) = (self.quarantined(key)?, self.served(key)?);
        if exists(&from).await? {
            return move_file(&from, &to).await;
        }
        if exists(&to).await? {
            return Ok(());
        }
        Err(StorageError::NotFound(key.to_string()))
    }

    async fn delete_quarantined(&self, key: &str) -> Result<(), StorageError> {
        remove(&self.quarantined(key)?).await
    }
}

/// Total size and count of the files under `dir`. A root that does not exist
/// yet is an empty storage, not an error.
fn walk(dir: &Path) -> Result<StorageUsage, StorageError> {
    let mut usage = StorageUsage::default();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                usage.bytes += entry.metadata()?.len();
                usage.objects += 1;
            }
        }
    }
    Ok(usage)
}
