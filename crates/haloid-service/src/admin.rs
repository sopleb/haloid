//! Moderation and administration.
//!
//! Moderators (and administrators) work the moderation queue. Everything
//! else here — settings, users, storages, deleted files, the audit log — is
//! for administrators only. Every action that changes something is written
//! to the audit log with who did it and from where.

use crate::files::Files;
use crate::library::Listed;
use crate::settings::SettingsHandle;
use crate::upload::Uploads;
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::account::{User, UserStatus};
use haloid_core::auth::{Actor, Role};
use haloid_core::ids::StorageId;
use haloid_core::pagination::{Cursor, FileSort, Page, clamp_limit};
use haloid_core::storage::{StorageKind, StorageRecord};
use haloid_db::files::{FileQuery, FileScope, Visibility};
use haloid_db::moderation::{self, Decision};
use haloid_db::{Db, audit, files, jobs, storages, users};
use haloid_storage::backend::StorageBackend;
use haloid_storage::pool::StoragePool;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

pub use haloid_db::storages::StorageDetails;

/// The job kind that destroys a file, shared with `haloid-jobs`.
pub const PURGE_FILE_JOB: &str = "purge_file";

/// Who is acting, and from where, for the audit log.
#[derive(Debug, Clone, Copy)]
pub struct By {
    pub actor: Actor,
    pub ip: Option<IpAddr>,
}

/// A file waiting for a decision.
#[derive(Debug, Clone)]
pub struct QueuedFile {
    pub listed: Listed,
    pub reason: Option<String>,
    pub queued_at: DateTime<Utc>,
}

/// One setting as the admin page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingView {
    pub key: String,
    pub value: String,
    pub default: String,
    /// Set in the environment, and so read-only here.
    pub pinned: bool,
}

pub struct Admin {
    db: Db,
    settings: SettingsHandle,
    uploads: Arc<Uploads>,
    files: Files,
    licensing: Option<Arc<crate::licensing::Licensing>>,
    backends: Vec<Arc<dyn StorageBackend>>,
    pool: Option<Arc<StoragePool>>,
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

fn internal(error: std::io::Error) -> DomainError {
    DomainError::Internal(Box::new(error))
}

fn moderator(actor: Actor) -> Result<(), DomainError> {
    match actor {
        Actor::Guest => Err(DomainError::Unauthenticated),
        a if a.can_moderate() => Ok(()),
        _ => Err(DomainError::Forbidden),
    }
}

fn admin(actor: Actor) -> Result<(), DomainError> {
    match actor {
        Actor::Guest => Err(DomainError::Unauthenticated),
        a if a.is_admin() => Ok(()),
        _ => Err(DomainError::Forbidden),
    }
}

impl Admin {
    pub fn new(db: Db, settings: SettingsHandle, uploads: Arc<Uploads>, files: Files) -> Self {
        Self {
            db,
            settings,
            uploads,
            files,
            licensing: None,
            backends: Vec::new(),
            pool: None,
        }
    }

    /// The pool in use, rebuilt after every storage change so it applies
    /// without a restart.
    pub fn with_storage_pool(mut self, pool: Arc<StoragePool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Would this storage build as configured, credentials included? Asked
    /// before saving, so a storage that cannot work is refused rather than
    /// saved and then missing from the pool.
    fn check_builds(&self, details: &StorageDetails) -> Result<(), DomainError> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        let now = Utc::now();
        let record = StorageRecord {
            id: StorageId(0),
            name: details.name.clone(),
            kind: details.kind,
            config: details.config.clone(),
            url_prefix: details.url_prefix.clone(),
            path_mode: details.path_mode,
            path_template: details.path_template.clone(),
            capacity_bytes: details.capacity_bytes,
            used_bytes: 0,
            file_count: 0,
            is_active: details.is_active,
            priority: details.priority,
            created_at: now,
            updated_at: now,
            retired_at: None,
        };
        pool.check(&record)
            .map_err(|error| DomainError::invalid_param("config", error.to_string()))
    }

    /// Put the storages as the database now has them into use.
    async fn reload_storages(&self) -> Result<(), DomainError> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        let records = storages::list(&self.db).await.map_err(database)?;
        pool.reload(&records)?;
        Ok(())
    }

    /// The licence, and the kinds of storage this build has beyond local.
    pub fn with_pro(
        mut self,
        licensing: Arc<crate::licensing::Licensing>,
        backends: Vec<Arc<dyn StorageBackend>>,
    ) -> Self {
        self.licensing = Some(licensing);
        self.backends = backends;
        self
    }

    /// A storage configuration that would fail to load is refused now,
    /// rather than taking the server down at its next start. Any kind but
    /// local is Pro: this build must have it, and the licence must allow
    /// putting it to new use. A storage that is already external may still
    /// be edited after a trial lapses, so its operator can keep it working.
    fn check_storage(
        &self,
        details: &StorageDetails,
        was: Option<StorageKind>,
    ) -> Result<(), DomainError> {
        if details.name.trim().is_empty() {
            return Err(DomainError::invalid_param("name", "is required"));
        }
        if details.kind == StorageKind::Local {
            if serde_json::from_str::<serde_json::Value>(&details.config).is_err() {
                return Err(DomainError::invalid_param("config", "is not JSON"));
            }
            return Ok(());
        }
        let backend = self
            .backends
            .iter()
            .find(|b| b.kind() == details.kind)
            .ok_or(DomainError::ProRequired("external storage"))?;
        let pro = self
            .licensing
            .as_ref()
            .is_some_and(|l| l.entitlements(Utc::now()).pro);
        if !pro && was != Some(details.kind) {
            return Err(DomainError::ProRequired("external storage"));
        }
        backend
            .check_config(&details.config)
            .map_err(|reason| DomainError::invalid_param("config", reason))
    }

    async fn audit(
        &self,
        by: By,
        action: &str,
        target: Option<(&str, u64)>,
        detail: serde_json::Value,
    ) -> Result<(), DomainError> {
        audit::record(
            &self.db,
            &audit::NewEntry {
                actor: by.actor.user_id(),
                action,
                target: target.map(|t| t.0),
                target_id: target.map(|t| t.1),
                detail,
                ip: by.ip,
            },
        )
        .await
        .map_err(database)
    }

    async fn file(&self, id_encoded: &str) -> Result<haloid_core::files::MediaFile, DomainError> {
        files::find_by_id_encoded(&self.db, id_encoded)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("file"))
    }

    // Moderation

    /// Files waiting for review, oldest first. `after` is the last file id
    /// of the previous page.
    pub async fn queue(
        &self,
        actor: Actor,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<QueuedFile>, Option<String>), DomainError> {
        moderator(actor)?;
        let after = match after {
            Some(id) => Some(self.file(id).await?.id),
            None => None,
        };
        let limit = clamp_limit(limit);
        let queued = moderation::pending(&self.db, after, limit)
            .await
            .map_err(database)?;
        let mut out = Vec::with_capacity(queued.len());
        for item in queued {
            if let Some(file) = files::find_by_id(&self.db, item.file_id)
                .await
                .map_err(database)?
            {
                out.push(QueuedFile {
                    listed: Listed {
                        urls: self.uploads.urls(&file, None),
                        file,
                    },
                    reason: item.reason,
                    queued_at: item.queued_at,
                });
            }
        }
        let next = (out.len() == limit as usize)
            .then(|| out.last().map(|q| q.listed.file.id_encoded.to_string()))
            .flatten();
        Ok((out, next))
    }

    /// Publish a held file: its bytes leave quarantine and it is listed.
    pub async fn approve(
        &self,
        by: By,
        id_encoded: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        moderator(by.actor)?;
        let file = self.file(id_encoded).await?;
        if file.deletion.is_some() {
            return Err(DomainError::NotFound("file"));
        }
        // Bytes first: an approved row whose bytes are still quarantined
        // would be a broken file in a gallery.
        self.files.release(&file).await?;
        files::set_approved(&self.db, file.id, true)
            .await
            .map_err(database)?;
        moderation::decide(
            &self.db,
            file.id,
            Decision::Approved,
            by.actor.user_id(),
            None,
            now,
        )
        .await
        .map_err(database)?;
        self.audit(
            by,
            "file.approve",
            Some(("file", file.id.0)),
            serde_json::json!({}),
        )
        .await
    }

    /// Refuse a held file: deleted, with reason `moderation`, and retained
    /// like any deletion.
    pub async fn deny(
        &self,
        by: By,
        id_encoded: &str,
        note: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        moderator(by.actor)?;
        let file = self.file(id_encoded).await?;
        self.files
            .delete(id_encoded, crate::files::DeleteAs::Actor(by.actor), now)
            .await?;
        moderation::decide(
            &self.db,
            file.id,
            Decision::Denied,
            by.actor.user_id(),
            note,
            now,
        )
        .await
        .map_err(database)?;
        self.audit(
            by,
            "file.deny",
            Some(("file", file.id.0)),
            serde_json::json!({ "note": note }),
        )
        .await
    }

    // Settings

    pub fn settings(&self, actor: Actor) -> Result<Vec<SettingView>, DomainError> {
        admin(actor)?;
        let current = self.settings.get().to_map();
        let defaults = haloid_core::settings::Settings::default().to_map();
        let pinned: Vec<&str> = self.settings.pinned_keys().collect();
        Ok(current
            .into_iter()
            .map(|(key, value)| SettingView {
                default: defaults.get(&key).cloned().unwrap_or_default(),
                pinned: pinned.contains(&key.as_str()),
                key,
                value,
            })
            .collect())
    }

    /// Change settings, all or nothing. Pinned and unknown keys refuse the
    /// whole write.
    pub async fn update_settings(
        &self,
        by: By,
        changes: &BTreeMap<String, String>,
    ) -> Result<Vec<SettingView>, DomainError> {
        admin(by.actor)?;
        if changes.is_empty() {
            return self.settings(by.actor);
        }
        self.settings.update(changes).await?;
        self.audit(
            by,
            "settings.update",
            None,
            serde_json::json!({ "changed": changes }),
        )
        .await?;
        self.settings(by.actor)
    }

    // Users

    pub async fn users(
        &self,
        actor: Actor,
        search: Option<&str>,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<User>, Option<String>), DomainError> {
        admin(actor)?;
        let after = match after {
            Some(id) => Some(
                users::find_by_id_encoded(&self.db, id)
                    .await
                    .map_err(database)?
                    .ok_or(DomainError::invalid_param("cursor", "no such user"))?
                    .id,
            ),
            None => None,
        };
        let limit = clamp_limit(limit);
        let list = users::list(&self.db, search, after, limit)
            .await
            .map_err(database)?;
        let next = (list.len() == limit as usize)
            .then(|| list.last().map(|u| u.id_encoded.to_string()))
            .flatten();
        Ok((list, next))
    }

    async fn user(&self, id_encoded: &str) -> Result<User, DomainError> {
        users::find_by_id_encoded(&self.db, id_encoded)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("user"))
    }

    /// Change a user's role or status. An administrator cannot demote or
    /// ban themselves: an instance must never be left with no way back in.
    pub async fn update_user(
        &self,
        by: By,
        id_encoded: &str,
        role: Option<Role>,
        status: Option<UserStatus>,
    ) -> Result<User, DomainError> {
        admin(by.actor)?;
        let user = self.user(id_encoded).await?;
        if Some(user.id) == by.actor.user_id()
            && (role.is_some_and(|r| r != Role::Admin)
                || status.is_some_and(|s| s != UserStatus::Valid))
        {
            return Err(DomainError::invalid_param(
                "role",
                "you cannot demote or ban yourself",
            ));
        }
        if let Some(role) = role
            && role != user.role
        {
            users::set_role(&self.db, user.id, role)
                .await
                .map_err(database)?;
            self.audit(
                by,
                "user.role",
                Some(("user", user.id.0)),
                serde_json::json!({ "from": user.role.as_str(), "to": role.as_str() }),
            )
            .await?;
        }
        if let Some(status) = status
            && status != user.status
        {
            users::set_status(&self.db, user.id, status)
                .await
                .map_err(database)?;
            // A ban ends every session at once.
            if status == UserStatus::Banned {
                haloid_db::sessions::delete_for_user(&self.db, user.id, None)
                    .await
                    .map_err(database)?;
            }
            self.audit(
                by,
                "user.status",
                Some(("user", user.id.0)),
                serde_json::json!({ "from": user.status.as_str(), "to": status.as_str() }),
            )
            .await?;
        }
        self.user(id_encoded).await
    }

    // Storages

    pub async fn storages(&self, actor: Actor) -> Result<Vec<StorageRecord>, DomainError> {
        admin(actor)?;
        storages::list(&self.db).await.map_err(database)
    }

    /// Add a storage. It is checked by building it, and in use as soon as
    /// it is saved.
    pub async fn create_storage(
        &self,
        by: By,
        details: &StorageDetails,
    ) -> Result<StorageRecord, DomainError> {
        admin(by.actor)?;
        self.check_storage(details, None)?;
        self.check_builds(details)?;
        let created = storages::create(&self.db, details).await?;
        self.reload_storages().await?;
        self.audit(
            by,
            "storage.create",
            Some(("storage", created.id.0)),
            serde_json::json!({ "name": created.name, "kind": created.kind.as_str() }),
        )
        .await?;
        Ok(created)
    }

    pub async fn update_storage(
        &self,
        by: By,
        id: StorageId,
        details: &StorageDetails,
    ) -> Result<StorageRecord, DomainError> {
        admin(by.actor)?;
        let was = storages::find_by_id(&self.db, id)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("storage"))?
            .kind;
        self.check_storage(details, Some(was))?;
        self.check_builds(details)?;
        if !storages::update(&self.db, id, details).await? {
            return Err(DomainError::NotFound("storage"));
        }
        self.reload_storages().await?;
        self.audit(
            by,
            "storage.update",
            Some(("storage", id.0)),
            serde_json::json!({}),
        )
        .await?;
        storages::find_by_id(&self.db, id)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("storage"))
    }

    /// Retire a storage: no new uploads, and it keeps serving what it has.
    pub async fn retire_storage(
        &self,
        by: By,
        id: StorageId,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        admin(by.actor)?;
        if !storages::retire(&self.db, id, now)
            .await
            .map_err(database)?
        {
            return Err(DomainError::NotFound("storage"));
        }
        self.reload_storages().await?;
        self.audit(
            by,
            "storage.retire",
            Some(("storage", id.0)),
            serde_json::json!({}),
        )
        .await
    }

    // Deleted files

    pub async fn deleted_files(
        &self,
        actor: Actor,
        cursor: Option<&str>,
        limit: Option<u32>,
        now: DateTime<Utc>,
    ) -> Result<Page<Listed, FileSort>, DomainError> {
        admin(actor)?;
        let sort = FileSort::Newest;
        let after = cursor.map(|c| Cursor::parse(c, sort)).transpose()?;
        let page = files::list(
            &self.db,
            &FileQuery {
                scope: FileScope::Everyone,
                visibility: Visibility::Deleted,
                include_nsfw: true,
                kind: None,
                sort,
                after,
                limit: clamp_limit(limit),
                now,
            },
        )
        .await?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(|file| Listed {
                    urls: self.uploads.urls(&file, None),
                    file,
                })
                .collect(),
            next: page.next,
        })
    }

    pub async fn restore(&self, by: By, id_encoded: &str) -> Result<(), DomainError> {
        admin(by.actor)?;
        let file = self.file(id_encoded).await?;
        self.files.restore(by.actor, id_encoded).await?;
        self.audit(
            by,
            "file.restore",
            Some(("file", file.id.0)),
            serde_json::json!({}),
        )
        .await
    }

    /// Destroy a file, by queueing the `purge_file` job: a storage failure
    /// halfway through stays retryable.
    pub async fn purge(
        &self,
        by: By,
        id_encoded: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        admin(by.actor)?;
        let file = self.file(id_encoded).await?;
        let key = format!("{PURGE_FILE_JOB}:{}", file.id.0);
        jobs::enqueue(
            &self.db,
            &jobs::NewJob {
                kind: PURGE_FILE_JOB,
                payload: serde_json::json!({ "file_id": file.id.0 }),
                dedupe_key: Some(&key),
                run_at: now,
                max_attempts: 10,
            },
        )
        .await
        .map_err(database)?;
        self.audit(
            by,
            "file.purge",
            Some(("file", file.id.0)),
            serde_json::json!({}),
        )
        .await
    }

    // Watermark

    /// The most a watermark image may weigh.
    pub const MAX_WATERMARK_BYTES: usize = 5 * 1024 * 1024;

    /// Replace the watermark image. It must be a still image the media
    /// tools can read; a PNG with transparency works best.
    pub async fn set_watermark(&self, by: By, bytes: &[u8]) -> Result<(), DomainError> {
        admin(by.actor)?;
        if bytes.is_empty() {
            return Err(DomainError::EmptySource);
        }
        if bytes.len() > Self::MAX_WATERMARK_BYTES {
            return Err(DomainError::SizeLimit {
                size: bytes.len() as u64,
                limit: Self::MAX_WATERMARK_BYTES as u64,
            });
        }
        let target = self.uploads.watermark_path();
        let dir = target
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        tokio::fs::create_dir_all(&dir).await.map_err(internal)?;
        let staged = dir.join(format!("watermark.new-{}", std::process::id()));
        tokio::fs::write(&staged, bytes).await.map_err(internal)?;
        let checked = self.uploads.media().probe(&staged).await;
        let ok = matches!(&checked, Ok(p) if p.kind == haloid_core::media::MediaKind::Image && !p.is_animated);
        if !ok {
            let _ = tokio::fs::remove_file(&staged).await;
            return Err(DomainError::invalid_param(
                "watermark",
                "must be a still image",
            ));
        }
        tokio::fs::rename(&staged, &target)
            .await
            .map_err(internal)?;
        self.audit(
            by,
            "watermark.set",
            None,
            serde_json::json!({ "bytes": bytes.len() }),
        )
        .await
    }

    /// The watermark image, if one is set: its bytes and mime type.
    pub async fn watermark(&self, actor: Actor) -> Result<Option<(Vec<u8>, String)>, DomainError> {
        admin(actor)?;
        let path = self.uploads.watermark_path();
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let mime = self
                    .uploads
                    .media()
                    .probe(&path)
                    .await
                    .map(|p| p.mime)
                    .unwrap_or_else(|_| "application/octet-stream".into());
                Ok(Some((bytes, mime)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(internal(e)),
        }
    }

    pub async fn remove_watermark(&self, by: By) -> Result<(), DomainError> {
        admin(by.actor)?;
        match tokio::fs::remove_file(self.uploads.watermark_path()).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(internal(e)),
        }
        self.audit(by, "watermark.remove", None, serde_json::json!({}))
            .await
    }

    // Audit

    pub async fn audit_log(
        &self,
        actor: Actor,
        before: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<audit::Entry>, DomainError> {
        admin(actor)?;
        audit::list(&self.db, before, clamp_limit(limit))
            .await
            .map_err(database)
    }
}
