//! The storages repository.
//!
//! Credentials are never stored here — see [`StorageRecord`]. A storage is
//! retired rather than deleted: it stops taking uploads but keeps serving the
//! files it holds, and the schema refuses to drop a row files still point at.
//!
//! `used_bytes` and `file_count` are maintained by the files repository and
//! count bytes physically held, deleted-but-unpurged files included.

use crate::error::{DbError, unique_violation};
use crate::{Db, decode_enum};
use chrono::{DateTime, Utc};
use haloid_core::ids::StorageId;
use haloid_core::storage::{StorageKind, StoragePathMode, StorageRecord};

/// Every administrator-editable field, for creating or replacing a storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDetails {
    /// Unique, case-insensitively; also the key for its credentials in the
    /// environment.
    pub name: String,
    pub kind: StorageKind,
    /// JSON text, interpreted by `haloid-storage`.
    pub config: String,
    pub url_prefix: String,
    pub path_mode: StoragePathMode,
    pub path_template: Option<String>,
    pub capacity_bytes: Option<u64>,
    pub is_active: bool,
    pub priority: i32,
}

// The JSON column is LONGTEXT with a binary collation, which the driver
// reports as BLOB; the cast makes it text.
const COLUMNS: &str = "s.id, s.name, s.kind, CAST(s.config AS CHAR CHARACTER SET utf8mb4) AS config, \
     s.url_prefix, s.path_mode, s.path_template, s.capacity_bytes, s.used_bytes, s.file_count, \
     s.is_active, s.priority, s.created_at, s.updated_at, s.deleted_at";

#[derive(sqlx::FromRow)]
struct StorageRow {
    id: u64,
    name: String,
    kind: String,
    config: String,
    url_prefix: String,
    path_mode: String,
    path_template: Option<String>,
    capacity_bytes: Option<u64>,
    used_bytes: u64,
    file_count: u64,
    is_active: bool,
    priority: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

impl StorageRow {
    fn into_domain(self) -> Result<StorageRecord, sqlx::Error> {
        Ok(StorageRecord {
            id: StorageId(self.id),
            name: self.name,
            kind: decode_enum("kind", &self.kind)?,
            config: self.config,
            url_prefix: self.url_prefix,
            path_mode: decode_enum("path_mode", &self.path_mode)?,
            path_template: self.path_template,
            capacity_bytes: self.capacity_bytes,
            used_bytes: self.used_bytes,
            file_count: self.file_count,
            is_active: self.is_active,
            priority: self.priority,
            created_at: self.created_at,
            updated_at: self.updated_at,
            retired_at: self.deleted_at,
        })
    }
}

fn conflict(error: sqlx::Error) -> DbError {
    if unique_violation(&error).as_deref() == Some("uq_storages_name") {
        DbError::Conflict("name")
    } else {
        error.into()
    }
}

pub async fn create(db: &Db, details: &StorageDetails) -> Result<StorageRecord, DbError> {
    let done = sqlx::query(
        "INSERT INTO ih_storages
           (name, kind, config, url_prefix, path_mode, path_template, capacity_bytes,
            is_active, priority)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&details.name)
    .bind(details.kind.as_str())
    .bind(&details.config)
    .bind(&details.url_prefix)
    .bind(details.path_mode.as_str())
    .bind(&details.path_template)
    .bind(details.capacity_bytes)
    .bind(details.is_active)
    .bind(details.priority)
    .execute(db)
    .await
    .map_err(conflict)?;
    find_by_id(db, StorageId(done.last_insert_id()))
        .await?
        .ok_or(DbError::Sqlx(sqlx::Error::RowNotFound))
}

pub async fn find_by_id(db: &Db, id: StorageId) -> Result<Option<StorageRecord>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_storages s WHERE s.id = ?");
    sqlx::query_as::<_, StorageRow>(&sql)
        .bind(id.0)
        .fetch_optional(db)
        .await?
        .map(StorageRow::into_domain)
        .transpose()
}

/// Every storage, retired ones included — they still serve their files.
pub async fn list(db: &Db) -> Result<Vec<StorageRecord>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_storages s ORDER BY s.id");
    let rows: Vec<StorageRow> = sqlx::query_as(&sql).fetch_all(db).await?;
    rows.into_iter().map(StorageRow::into_domain).collect()
}

/// Returns whether the storage exists.
pub async fn update(db: &Db, id: StorageId, details: &StorageDetails) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE ih_storages
         SET name = ?, kind = ?, config = ?, url_prefix = ?, path_mode = ?, path_template = ?,
             capacity_bytes = ?, is_active = ?, priority = ?
         WHERE id = ?",
    )
    .bind(&details.name)
    .bind(details.kind.as_str())
    .bind(&details.config)
    .bind(&details.url_prefix)
    .bind(details.path_mode.as_str())
    .bind(&details.path_template)
    .bind(details.capacity_bytes)
    .bind(details.is_active)
    .bind(details.priority)
    .bind(id.0)
    .execute(db)
    .await
    .map_err(conflict)?;
    Ok(done.rows_affected() > 0)
}

/// Take a storage in or out of rotation for new uploads. A retired storage
/// cannot be reactivated. Returns whether the change applied.
pub async fn set_active(db: &Db, id: StorageId, active: bool) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE ih_storages SET is_active = ? WHERE id = ? AND (deleted_at IS NULL OR ? = 0)",
    )
    .bind(active)
    .bind(id.0)
    .bind(active)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Retire a storage: no new uploads, ever, while it keeps serving what it
/// holds. Returns whether this call retired it.
pub async fn retire(db: &Db, id: StorageId, at: DateTime<Utc>) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE ih_storages SET deleted_at = ?, is_active = 0 WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(at)
    .bind(id.0)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Where an upload of `size` bytes should go: an active, unretired storage
/// with room, highest `priority` first, then unlimited capacity, then the
/// most free space. `None` means every storage is full — the pipeline's
/// `StorageFull`.
pub async fn pick_for_upload(db: &Db, size: u64) -> Result<Option<StorageRecord>, sqlx::Error> {
    let sql = format!(
        "SELECT {COLUMNS} FROM ih_storages s
         WHERE s.is_active = 1 AND s.deleted_at IS NULL
           AND (s.capacity_bytes IS NULL OR s.used_bytes + ? <= s.capacity_bytes)
         ORDER BY s.priority DESC,
                  s.capacity_bytes IS NULL DESC,
                  CAST(s.capacity_bytes AS SIGNED) - CAST(s.used_bytes AS SIGNED) DESC,
                  s.id
         LIMIT 1"
    );
    sqlx::query_as::<_, StorageRow>(&sql)
        .bind(size)
        .fetch_optional(db)
        .await?
        .map(StorageRow::into_domain)
        .transpose()
}
