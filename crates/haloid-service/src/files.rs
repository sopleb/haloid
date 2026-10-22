//! Deleting, restoring and purging files: the database and the bytes
//! together.
//!
//! Deletion is retained. Deleting a file records who and why, hides it from
//! every listing, and moves each of its objects into its storage's
//! quarantine, where nothing serves it — so access is gone from the URL
//! too, not just the gallery. Only an administrator can restore a file
//! (moving its objects back) or purge it (removing row and bytes for good).
//!
//! The database changes first, then the bytes. Every storage step is
//! idempotent, so a failed batch is fixed by running the same call again:
//! deleting an already-deleted file still re-runs its quarantine.

use crate::upload::object_keys;
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::auth::Actor;
use haloid_core::files::{DeleteReason, Deletion, MediaFile};
use haloid_core::ids::FileId;
use haloid_core::storage::Storage;
use haloid_db::{Db, files};
use haloid_storage::pool::StoragePool;
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub struct Files {
    db: Db,
    storages: Arc<StoragePool>,
}

/// How a deletion is authorised.
pub enum DeleteAs<'a> {
    /// The actor's own rights: owner, moderator or admin.
    Actor(Actor),
    /// Anyone holding the upload's one-shot delete token — usually a guest.
    Token(&'a str),
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

impl Files {
    pub fn new(db: Db, storages: Arc<StoragePool>) -> Self {
        Self { db, storages }
    }

    async fn find(&self, id_encoded: &str) -> Result<MediaFile, DomainError> {
        files::find_by_id_encoded(&self.db, id_encoded)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("file"))
    }

    fn storage(&self, file: &MediaFile) -> Result<Arc<dyn Storage>, DomainError> {
        file.storage_id
            .and_then(|id| self.storages.get(id))
            .ok_or_else(|| DomainError::Internal(format!("file {} has no storage", file.id).into()))
    }

    /// Delete a file, retaining it for audit, and take its bytes out of
    /// public reach.
    pub async fn delete(
        &self,
        id_encoded: &str,
        authority: DeleteAs<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        let file = self.find(id_encoded).await?;
        let (by, reason) = match authority {
            DeleteAs::Actor(actor) => {
                let reason = if actor.owns(file.user_id) {
                    DeleteReason::Owner
                } else if actor.is_admin() {
                    DeleteReason::Admin
                } else if actor.can_moderate() {
                    DeleteReason::Moderation
                } else {
                    // Not found, rather than forbidden: whether someone
                    // else's file exists is not this caller's business.
                    return Err(DomainError::NotFound("file"));
                };
                (actor.user_id(), reason)
            }
            DeleteAs::Token(token) => {
                let stored = files::delete_token(&self.db, file.id)
                    .await
                    .map_err(database)?;
                let matches = stored
                    .as_deref()
                    .is_some_and(|stored| bool::from(stored.as_bytes().ct_eq(token.as_bytes())));
                if !matches {
                    return Err(DomainError::NotFound("file"));
                }
                (None, DeleteReason::DeleteToken)
            }
        };

        files::soft_delete(
            &self.db,
            file.id,
            &Deletion {
                at: now,
                by,
                reason,
            },
        )
        .await
        .map_err(database)?;
        self.quarantine(&file).await
    }

    /// Move every object of a file into its storage's quarantine, where
    /// nothing serves it. Idempotent: an object already there is fine.
    pub(crate) async fn quarantine(&self, file: &MediaFile) -> Result<(), DomainError> {
        let storage = self.storage(file)?;
        for (_, key) in object_keys(file)? {
            match storage.quarantine(&key).await {
                Ok(()) | Err(haloid_core::StorageError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// Move a file's objects back out of quarantine, so they are served
    /// again: an approval, or a restore. Idempotent like `quarantine`.
    pub(crate) async fn release(&self, file: &MediaFile) -> Result<(), DomainError> {
        let storage = self.storage(file)?;
        for (_, key) in object_keys(file)? {
            match storage.release(&key).await {
                Ok(()) | Err(haloid_core::StorageError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// The same, for files a database operation already soft-deleted — an
    /// album deleted with its contents.
    pub(crate) async fn quarantine_ids(&self, ids: &[FileId]) -> Result<(), DomainError> {
        for id in ids {
            if let Some(file) = files::find_by_id(&self.db, *id).await.map_err(database)? {
                self.quarantine(&file).await?;
            }
        }
        Ok(())
    }

    /// Undo a deletion. Administrators only.
    pub async fn restore(&self, actor: Actor, id_encoded: &str) -> Result<(), DomainError> {
        if !actor.is_admin() {
            return Err(DomainError::Forbidden);
        }
        let file = self.find(id_encoded).await?;
        let storage = self.storage(&file)?;
        // Bytes first here: a restored row whose bytes are still in
        // quarantine would be a broken file in someone's gallery. A file
        // still awaiting moderation stays quarantined: restoring the row
        // must not publish what no moderator has approved.
        let keys = if file.is_approved {
            object_keys(&file)?
        } else {
            Vec::new()
        };
        for (_, key) in keys {
            match storage.release(&key).await {
                Ok(()) | Err(haloid_core::StorageError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        files::restore(&self.db, file.id).await.map_err(database)?;
        Ok(())
    }

    /// Destroy a file, live or deleted: row and bytes, for good.
    /// Administrators only.
    pub async fn purge(&self, actor: Actor, id_encoded: &str) -> Result<(), DomainError> {
        if !actor.is_admin() {
            return Err(DomainError::Forbidden);
        }
        let file = self.find(id_encoded).await?;
        self.destroy(&file).await
    }

    /// Purge by row id, for the `PurgeFile` job. The job is only ever
    /// queued by an administrator's purge, so the rights were checked when
    /// it was queued. A file already gone is done.
    pub async fn purge_queued(&self, id: FileId) -> Result<(), DomainError> {
        match files::find_by_id(&self.db, id).await.map_err(database)? {
            Some(file) => self.destroy(&file).await,
            None => Ok(()),
        }
    }

    /// Expire a file: its requested expiry has passed. A deletion like any
    /// other — retained, quarantined, purged only by an administrator.
    /// Returns whether it was still live.
    pub async fn expire(&self, id: FileId, now: DateTime<Utc>) -> Result<bool, DomainError> {
        let Some(file) = files::find_by_id(&self.db, id).await.map_err(database)? else {
            return Ok(false);
        };
        if file.deletion.is_some() {
            return Ok(false);
        }
        files::soft_delete(
            &self.db,
            file.id,
            &Deletion {
                at: now,
                by: None,
                reason: DeleteReason::Expired,
            },
        )
        .await
        .map_err(database)?;
        self.quarantine(&file).await?;
        Ok(true)
    }

    /// Row and bytes, for good.
    async fn destroy(&self, file: &MediaFile) -> Result<(), DomainError> {
        let storage = self.storage(file)?;
        let keys = object_keys(file)?;
        files::purge(&self.db, file.id).await.map_err(database)?;
        // Both places, so a live file and a quarantined one purge alike.
        for (_, key) in keys {
            storage.delete(&key).await?;
            storage.delete_quarantined(&key).await?;
        }
        Ok(())
    }
}
