//! The files repository.
//!
//! A file's life: [`create_pending`] inserts the row before the bytes are
//! uploaded (the pipeline needs its public id for the storage path), and
//! [`mark_ready`] publishes it once they have landed. Nothing lists or
//! serves a pending row.
//!
//! **Deletion is retained.** [`soft_delete`] records when, by whom and why,
//! and the row and its bytes stay until an administrator calls [`purge`]. A
//! deleted file is invisible to every listing except the administrators'
//! deleted view, so "what happened to this file" stays answerable.
//!
//! Counters (the owner's `file_count`/`disk_used`, the album's and the
//! category's `file_count`) count live files: they rise when a file becomes
//! ready and fall when it is deleted, so they always match what the user can
//! see. Every change is `SET x = x + ?`, floored at zero, never
//! read-modify-write.

use crate::error::{DbError, unique_violation};
use crate::{Db, decode_enum, decode_error, ip};
use chrono::{DateTime, TimeZone, Utc};
use haloid_core::files::{Deletion, MediaFile};
use haloid_core::ids::{AlbumId, CategoryId, FileId, StorageId, TagId, UserId};
use haloid_core::media::{Chain, MediaKind};
use haloid_core::pagination::{Cursor, FileSort, Page};
use haloid_core::storage::StoragePathMode;
use sqlx::{MySql, MySqlConnection, QueryBuilder};
use std::net::IpAddr;

/// Everything the upload pipeline knows before the bytes are stored.
#[derive(Debug, Clone)]
pub struct NewFile<'a> {
    pub kind: MediaKind,
    pub name: &'a str,
    pub extension: &'a str,
    pub original_extension: Option<&'a str>,
    pub mime: &'a str,
    pub original_filename: Option<&'a str>,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
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
    pub checksum: &'a str,
    pub source_checksum: Option<&'a str>,
    pub sha256: Option<&'a str>,
    pub storage_id: Option<StorageId>,
    pub storage_mode: StoragePathMode,
    pub user_id: Option<UserId>,
    pub album_id: Option<AlbumId>,
    pub category_id: Option<CategoryId>,
    pub nsfw: bool,
    pub is_approved: bool,
    pub is_360: bool,
    /// EXIF as JSON text, already filtered of MakerNote and oversized tags.
    pub exif: Option<&'a str>,
    pub uploader_ip: Option<IpAddr>,
    pub delete_token: Option<&'a str>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// The fields an owner may edit after upload. A full replacement: the
/// service merges a partial update onto the current values first.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDetails {
    pub title: Option<String>,
    pub description: Option<String>,
    pub nsfw: bool,
    pub category_id: Option<CategoryId>,
}

/// Which files a list draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileScope {
    Everyone,
    User(UserId),
    Album(AlbumId),
    Category(CategoryId),
    Tag(TagId),
}

/// How much of the scope the viewer may see. Choosing the right one is the
/// service's authorisation decision; this layer only applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// The owner, or a moderator: every live file, approved or not.
    Owner,
    /// Someone allowed to view the scope directly — an album they have the
    /// link (or password) to. Approved live files only.
    Viewer,
    /// Public listings: explore, search, a public profile. As `Viewer`, and
    /// additionally excludes files in non-public albums (files inherit their
    /// album's privacy) and files of private profiles.
    Listed,
    /// Administrators' audit view: deleted files only, awaiting purge or
    /// review.
    Deleted,
}

#[derive(Debug, Clone)]
pub struct FileQuery {
    pub scope: FileScope,
    pub visibility: Visibility,
    pub include_nsfw: bool,
    pub kind: Option<MediaKind>,
    pub sort: FileSort,
    pub after: Option<Cursor<FileSort>>,
    pub limit: u32,
    /// Expiry is evaluated against this, so an expired file disappears the
    /// moment it expires rather than when the expiry job next runs.
    pub now: DateTime<Utc>,
}

const COLUMNS: &str = "f.id, f.id_encoded, f.kind, f.state, f.name, f.extension, f.original_extension, f.mime, \
     f.original_filename, f.title, f.description, f.size, f.width, f.height, f.duration_ms, \
     f.is_animated, f.frame_count, f.chain, f.thumb_size, f.medium_size, f.frame_size, \
     f.checksum, f.source_checksum, f.sha256, f.storage_id, f.storage_mode, f.storage_path, \
     f.user_id, f.album_id, f.album_position, f.category_id, f.nsfw, f.is_approved, f.is_360, \
     f.view_count, f.like_count, f.expires_at, f.created_at, f.updated_at, f.deleted_at, \
     f.deleted_by, f.delete_reason";

#[derive(sqlx::FromRow)]
struct FileRow {
    id: u64,
    id_encoded: String,
    kind: String,
    state: String,
    name: String,
    extension: String,
    original_extension: Option<String>,
    mime: String,
    original_filename: Option<String>,
    title: Option<String>,
    description: Option<String>,
    size: u64,
    width: u32,
    height: u32,
    duration_ms: Option<u32>,
    is_animated: bool,
    frame_count: Option<u32>,
    chain: u8,
    thumb_size: Option<u64>,
    medium_size: Option<u64>,
    frame_size: Option<u64>,
    checksum: String,
    source_checksum: Option<String>,
    sha256: Option<String>,
    storage_id: Option<u64>,
    storage_mode: String,
    storage_path: String,
    user_id: Option<u64>,
    album_id: Option<u64>,
    album_position: Option<u32>,
    category_id: Option<u32>,
    nsfw: bool,
    is_approved: bool,
    is_360: bool,
    view_count: u64,
    like_count: u32,
    expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
    deleted_by: Option<u64>,
    delete_reason: Option<String>,
}

impl FileRow {
    fn into_domain(self) -> Result<MediaFile, sqlx::Error> {
        let deletion = match (self.deleted_at, self.delete_reason) {
            (Some(at), Some(reason)) => Some(Deletion {
                at,
                by: self.deleted_by.map(UserId),
                reason: decode_enum("delete_reason", &reason)?,
            }),
            // Only this module writes `deleted_at`, always with a reason.
            (Some(_), None) => {
                return Err(decode_error("delete_reason", "deleted without a reason"));
            }
            (None, _) => None,
        };
        Ok(MediaFile {
            id: FileId(self.id),
            id_encoded: self.id_encoded.into(),
            kind: decode_enum("kind", &self.kind)?,
            state: decode_enum("state", &self.state)?,
            name: self.name,
            extension: self.extension,
            original_extension: self.original_extension,
            mime: self.mime,
            original_filename: self.original_filename,
            title: self.title,
            description: self.description,
            size: self.size,
            width: self.width,
            height: self.height,
            duration_ms: self.duration_ms,
            is_animated: self.is_animated,
            frame_count: self.frame_count,
            chain: Chain::from_bits(self.chain),
            thumb_size: self.thumb_size,
            medium_size: self.medium_size,
            frame_size: self.frame_size,
            checksum: self.checksum,
            source_checksum: self.source_checksum,
            sha256: self.sha256,
            storage_id: self.storage_id.map(StorageId),
            storage_mode: decode_enum("storage_mode", &self.storage_mode)?,
            storage_path: self.storage_path,
            user_id: self.user_id.map(UserId),
            album_id: self.album_id.map(AlbumId),
            album_position: self.album_position,
            category_id: self.category_id.map(CategoryId),
            nsfw: self.nsfw,
            is_approved: self.is_approved,
            is_360: self.is_360,
            view_count: self.view_count,
            like_count: self.like_count,
            expires_at: self.expires_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
            deletion,
        })
    }
}

const ID_ATTEMPTS: usize = 3;

/// The next manual-order position in an album, as a scalar subquery.
///
/// Wrapped in a derived table because MariaDB refuses a statement that
/// writes `ih_files` while a plain subquery reads it (error 1093). Two
/// concurrent appends can draw the same position; the id tie-breaker keeps
/// the order stable, and a reorder rewrites positions densely.
const NEXT_POSITION: &str = "(SELECT p FROM (SELECT COALESCE(MAX(album_position), 0) + 1 AS p \
     FROM ih_files WHERE album_id = ?) AS next_position)";

/// Insert a pending file. `storage_path` builds the storage key from the
/// public id this function draws, and is called again if that id collides.
pub async fn create_pending(
    db: &Db,
    new: &NewFile<'_>,
    storage_path: impl Fn(&str) -> String,
) -> Result<MediaFile, DbError> {
    let album = new.album_id.map(|id| id.0);
    for _ in 0..ID_ATTEMPTS {
        let id_encoded = haloid_id::generate_public_id();
        let sql = format!(
            "INSERT INTO ih_files
               (id_encoded, kind, state, name, extension, original_extension, mime,
                original_filename, title,
                description, size, width, height, duration_ms, is_animated, frame_count, chain,
                thumb_size, medium_size, frame_size, checksum, source_checksum, sha256,
                storage_id, storage_mode, storage_path, user_id, album_id, album_position,
                category_id, nsfw, is_approved, is_360, exif, uploader_ip, delete_token,
                expires_at)
             VALUES (?, ?, 'pending', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                     ?, ?, ?, ?, ?, IF(? IS NULL, NULL, {NEXT_POSITION}),
                     ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        let result = sqlx::query(&sql)
            .bind(&id_encoded)
            .bind(new.kind.as_str())
            .bind(new.name)
            .bind(new.extension)
            .bind(new.original_extension)
            .bind(new.mime)
            .bind(new.original_filename)
            .bind(new.title)
            .bind(new.description)
            .bind(new.size)
            .bind(new.width)
            .bind(new.height)
            .bind(new.duration_ms)
            .bind(new.is_animated)
            .bind(new.frame_count)
            .bind(new.chain.bits())
            .bind(new.thumb_size)
            .bind(new.medium_size)
            .bind(new.frame_size)
            .bind(new.checksum)
            .bind(new.source_checksum)
            .bind(new.sha256)
            .bind(new.storage_id.map(|id| id.0))
            .bind(new.storage_mode.as_str())
            .bind(storage_path(&id_encoded))
            .bind(new.user_id.map(|id| id.0))
            .bind(album)
            .bind(album)
            .bind(album)
            .bind(new.category_id.map(|id| id.0))
            .bind(new.nsfw)
            .bind(new.is_approved)
            .bind(new.is_360)
            .bind(new.exif)
            .bind(new.uploader_ip.map(ip::to_bytes))
            .bind(new.delete_token)
            .bind(new.expires_at)
            .execute(db)
            .await;

        match result {
            Ok(done) => {
                return find_by_id(db, FileId(done.last_insert_id()))
                    .await?
                    .ok_or(DbError::Sqlx(sqlx::Error::RowNotFound));
            }
            Err(e) if unique_violation(&e).as_deref() == Some("uq_files_id_encoded") => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(DbError::Conflict("id_encoded"))
}

pub async fn find_by_id(db: &Db, id: FileId) -> Result<Option<MediaFile>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_files f WHERE f.id = ?");
    sqlx::query_as::<_, FileRow>(&sql)
        .bind(id.0)
        .fetch_optional(db)
        .await?
        .map(FileRow::into_domain)
        .transpose()
}

/// Any state, deleted or not: whether the caller may see it is the caller's
/// call, via [`MediaFile::is_live`] and the moderation and privacy rules.
pub async fn find_by_id_encoded(
    db: &Db,
    id_encoded: &str,
) -> Result<Option<MediaFile>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_files f WHERE f.id_encoded = ?");
    sqlx::query_as::<_, FileRow>(&sql)
        .bind(id_encoded)
        .fetch_optional(db)
        .await?
        .map(FileRow::into_domain)
        .transpose()
}

/// An existing live upload with the same source bytes, for duplicate
/// detection. Scoped to one owner; `None` scopes it to guest uploads, all of
/// which are public anyway.
pub async fn find_duplicate(
    db: &Db,
    source_checksum: &str,
    owner: Option<UserId>,
) -> Result<Option<MediaFile>, sqlx::Error> {
    let sql = format!(
        "SELECT {COLUMNS} FROM ih_files f
         WHERE f.source_checksum = ? AND f.user_id <=> ?
           AND f.state = 'ready' AND f.deleted_at IS NULL
         ORDER BY f.id LIMIT 1"
    );
    sqlx::query_as::<_, FileRow>(&sql)
        .bind(source_checksum)
        .bind(owner.map(|id| id.0))
        .fetch_optional(db)
        .await?
        .map(FileRow::into_domain)
        .transpose()
}

/// The one-shot delete token, for the service to compare in constant time.
/// Looked up by file rather than by token: the token column is unindexed on
/// purpose, so a token alone finds nothing.
pub async fn delete_token(db: &Db, id: FileId) -> Result<Option<String>, sqlx::Error> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>("SELECT delete_token FROM ih_files WHERE id = ?")
            .bind(id.0)
            .fetch_optional(db)
            .await?
            .flatten(),
    )
}

/// The stored EXIF JSON.
pub async fn exif(db: &Db, id: FileId) -> Result<Option<String>, sqlx::Error> {
    // MariaDB's JSON is LONGTEXT with a binary collation, which the driver
    // reports as BLOB; the cast makes it text again.
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT CAST(exif AS CHAR CHARACTER SET utf8mb4) FROM ih_files WHERE id = ?",
    )
    .bind(id.0)
    .fetch_optional(db)
    .await?
    .flatten())
}

/// Who uploaded it, for moderators and the audit trail.
pub async fn uploader_ip(db: &Db, id: FileId) -> Result<Option<IpAddr>, sqlx::Error> {
    Ok(
        sqlx::query_scalar::<_, Option<Vec<u8>>>("SELECT uploader_ip FROM ih_files WHERE id = ?")
            .bind(id.0)
            .fetch_optional(db)
            .await?
            .flatten()
            .and_then(|bytes| ip::from_bytes(&bytes)),
    )
}

/// What a file contributes to the counters, read under a row lock.
#[derive(sqlx::FromRow)]
struct CounterRow {
    id: u64,
    user_id: Option<u64>,
    storage_id: Option<u64>,
    album_id: Option<u64>,
    category_id: Option<u32>,
    bytes: i64,
    /// Ready: its bytes are on the storage, deleted or not.
    ready: bool,
    /// Ready and not deleted: what every user-facing counter counts.
    live: bool,
}

async fn lock_counters(
    conn: &mut MySqlConnection,
    id: FileId,
) -> Result<Option<CounterRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, user_id, storage_id, album_id, category_id,
                CAST(size + COALESCE(thumb_size, 0) + COALESCE(medium_size, 0)
                     + COALESCE(frame_size, 0) AS SIGNED) AS bytes,
                (state = 'ready') AS ready,
                (state = 'ready' AND deleted_at IS NULL) AS live
         FROM ih_files WHERE id = ? FOR UPDATE",
    )
    .bind(id.0)
    .fetch_optional(conn)
    .await
}

/// Add (`sign = 1`) or remove (`sign = -1`) one live file from every
/// user-facing counter it is part of: its owner's, its album's, its
/// category's and its tags'.
async fn count(conn: &mut MySqlConnection, row: &CounterRow, sign: i64) -> Result<(), sqlx::Error> {
    if let Some(user) = row.user_id {
        sqlx::query(
            "UPDATE ih_users
             SET file_count = GREATEST(CAST(file_count AS SIGNED) + ?, 0),
                 disk_used  = GREATEST(CAST(disk_used AS SIGNED) + ?, 0)
             WHERE id = ?",
        )
        .bind(sign)
        .bind(sign * row.bytes)
        .bind(user)
        .execute(&mut *conn)
        .await?;
    }
    if let Some(album) = row.album_id {
        count_album(conn, album, sign).await?;
    }
    if let Some(category) = row.category_id {
        count_category(conn, category, sign).await?;
    }
    sqlx::query(
        "UPDATE ih_tags t JOIN ih_file_tags ft ON ft.tag_id = t.id
         SET t.file_count = GREATEST(CAST(t.file_count AS SIGNED) + ?, 0)
         WHERE ft.file_id = ?",
    )
    .bind(sign)
    .bind(row.id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A storage's usage counts bytes it physically holds, so unlike [`count`]
/// it ignores deletion: it moves when bytes arrive and when a purge removes
/// them.
async fn count_storage(
    conn: &mut MySqlConnection,
    row: &CounterRow,
    sign: i64,
) -> Result<(), sqlx::Error> {
    if let Some(storage) = row.storage_id {
        sqlx::query(
            "UPDATE ih_storages
             SET used_bytes = GREATEST(CAST(used_bytes AS SIGNED) + ?, 0),
                 file_count = GREATEST(CAST(file_count AS SIGNED) + ?, 0)
             WHERE id = ?",
        )
        .bind(sign * row.bytes)
        .bind(sign)
        .bind(storage)
        .execute(conn)
        .await?;
    }
    Ok(())
}

async fn count_album(
    conn: &mut MySqlConnection,
    album: u64,
    delta: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ih_albums SET file_count = GREATEST(CAST(file_count AS SIGNED) + ?, 0)
         WHERE id = ?",
    )
    .bind(delta)
    .bind(album)
    .execute(conn)
    .await?;
    Ok(())
}

async fn count_category(
    conn: &mut MySqlConnection,
    category: u32,
    delta: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ih_categories SET file_count = GREATEST(CAST(file_count AS SIGNED) + ?, 0)
         WHERE id = ?",
    )
    .bind(delta)
    .bind(category)
    .execute(conn)
    .await?;
    Ok(())
}

/// Publish a pending file once its bytes are stored, and count it. Returns
/// whether this call published it: `false` if it was already ready, deleted
/// meanwhile, or missing.
pub async fn mark_ready(db: &Db, id: FileId) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let Some(row) = lock_counters(&mut tx, id).await? else {
        return Ok(false);
    };
    let done = sqlx::query(
        "UPDATE ih_files SET state = 'ready'
         WHERE id = ? AND state = 'pending' AND deleted_at IS NULL",
    )
    .bind(id.0)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    count(&mut tx, &row, 1).await?;
    count_storage(&mut tx, &row, 1).await?;
    tx.commit().await?;
    Ok(true)
}

/// Delete a file, retaining it. Records when, by whom and why; the row and
/// its bytes stay until an administrator purges them. Returns whether this
/// call deleted it, so a second delete never overwrites the first record.
///
/// Revoking access to the *bytes* — which a storage may still serve at a
/// public URL — is the storage layer's job, not this one's.
pub async fn soft_delete(db: &Db, id: FileId, deletion: &Deletion) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let deleted = soft_delete_in(&mut tx, id, deletion).await?;
    tx.commit().await?;
    Ok(deleted)
}

/// [`soft_delete`] inside a caller's transaction, for deleting an album with
/// its contents atomically.
pub(crate) async fn soft_delete_in(
    conn: &mut MySqlConnection,
    id: FileId,
    deletion: &Deletion,
) -> Result<bool, sqlx::Error> {
    let Some(row) = lock_counters(conn, id).await? else {
        return Ok(false);
    };
    let done = sqlx::query(
        "UPDATE ih_files SET deleted_at = ?, deleted_by = ?, delete_reason = ?
         WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(deletion.at)
    .bind(deletion.by.map(|id| id.0))
    .bind(deletion.reason.as_str())
    .bind(id.0)
    .execute(&mut *conn)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    if row.live {
        count(conn, &row, -1).await?;
    }
    Ok(true)
}

/// Undo a deletion: the administrators' restore. Clears the deletion record
/// and counts the file again if it was ready. Returns whether this call
/// restored it — `false` for a live or missing file.
///
/// Recording who restored it belongs in the audit log, which the service
/// writes; the row only ever describes its current deletion.
pub async fn restore(db: &Db, id: FileId) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let Some(row) = lock_counters(&mut tx, id).await? else {
        return Ok(false);
    };
    let done = sqlx::query(
        "UPDATE ih_files SET deleted_at = NULL, deleted_by = NULL, delete_reason = NULL
         WHERE id = ? AND deleted_at IS NOT NULL",
    )
    .bind(id.0)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    let ready: bool = sqlx::query_scalar("SELECT state = 'ready' FROM ih_files WHERE id = ?")
        .bind(id.0)
        .fetch_one(&mut *tx)
        .await?;
    if ready {
        count(&mut tx, &row, 1).await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// Hard-delete a file: the administrators' purge. Works on a deleted file
/// and on a live one alike, uncounting the latter.
///
/// Returns the file as it was, so the caller can remove its bytes. The row
/// goes first: if removing the bytes then fails they are an orphan for the
/// sweep, which is recoverable, whereas bytes removed under a surviving row
/// would be a broken file.
pub async fn purge(db: &Db, id: FileId) -> Result<Option<MediaFile>, sqlx::Error> {
    let mut tx = db.begin().await?;
    let Some(row) = lock_counters(&mut tx, id).await? else {
        return Ok(None);
    };
    let sql = format!("SELECT {COLUMNS} FROM ih_files f WHERE f.id = ?");
    let file = sqlx::query_as::<_, FileRow>(&sql)
        .bind(id.0)
        .fetch_one(&mut *tx)
        .await?
        .into_domain()?;

    if row.live {
        count(&mut tx, &row, -1).await?;
    }
    if row.ready {
        count_storage(&mut tx, &row, -1).await?;
    }
    sqlx::query("DELETE FROM ih_files WHERE id = ?")
        .bind(id.0)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(file))
}

/// Replace the editable fields. A category change moves the file between
/// category counters. Returns whether the file exists.
pub async fn update_details(
    db: &Db,
    id: FileId,
    details: &FileDetails,
) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let Some(row) = lock_counters(&mut tx, id).await? else {
        return Ok(false);
    };
    sqlx::query(
        "UPDATE ih_files SET title = ?, description = ?, nsfw = ?, category_id = ? WHERE id = ?",
    )
    .bind(&details.title)
    .bind(&details.description)
    .bind(details.nsfw)
    .bind(details.category_id.map(|id| id.0))
    .bind(id.0)
    .execute(&mut *tx)
    .await?;

    let new_category = details.category_id.map(|id| id.0);
    if row.live && row.category_id != new_category {
        if let Some(old) = row.category_id {
            count_category(&mut tx, old, -1).await?;
        }
        if let Some(new) = new_category {
            count_category(&mut tx, new, 1).await?;
        }
    }
    tx.commit().await?;
    Ok(true)
}

/// Moderation. Returns whether the file exists.
pub async fn set_approved(db: &Db, id: FileId, approved: bool) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("UPDATE ih_files SET is_approved = ? WHERE id = ?")
        .bind(approved)
        .bind(id.0)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Move a file into an album, at the end of its manual order, or out of any
/// album with `None`. Returns whether the file exists.
pub async fn move_to_album(
    db: &Db,
    id: FileId,
    album: Option<AlbumId>,
) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let Some(row) = lock_counters(&mut tx, id).await? else {
        return Ok(false);
    };
    let target = album.map(|a| a.0);
    if row.album_id == target {
        return Ok(true);
    }

    let sql = format!(
        "UPDATE ih_files SET album_id = ?, album_position = IF(? IS NULL, NULL, {NEXT_POSITION})
         WHERE id = ?"
    );
    sqlx::query(&sql)
        .bind(target)
        .bind(target)
        .bind(target)
        .bind(id.0)
        .execute(&mut *tx)
        .await?;

    if row.live {
        if let Some(old) = row.album_id {
            count_album(&mut tx, old, -1).await?;
        }
        if let Some(new) = target {
            count_album(&mut tx, new, 1).await?;
        }
    }
    tx.commit().await?;
    Ok(true)
}

/// Rearrange an album: `order` comes first, in the order given, and every
/// other file in the album follows in its current order. Ids not in the
/// album are ignored. Positions are rewritten densely from 1.
pub async fn set_album_order(db: &Db, album: AlbumId, order: &[FileId]) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    let current: Vec<u64> = sqlx::query_scalar(
        "SELECT id FROM ih_files WHERE album_id = ?
         ORDER BY album_position, id FOR UPDATE",
    )
    .bind(album.0)
    .fetch_all(&mut *tx)
    .await?;

    // A file named twice keeps its first place: `dedup` alone would only
    // catch repeats that happen to be adjacent.
    let mut arranged: Vec<u64> = Vec::with_capacity(current.len());
    for id in order.iter().map(|id| id.0) {
        if current.contains(&id) && !arranged.contains(&id) {
            arranged.push(id);
        }
    }
    let rest: Vec<u64> = current
        .iter()
        .copied()
        .filter(|id| !arranged.contains(id))
        .collect();
    arranged.extend(rest);

    for (position, id) in (1u32..).zip(arranged) {
        sqlx::query("UPDATE ih_files SET album_position = ? WHERE id = ?")
            .bind(position)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// Who a cover is chosen for: what they may see of the album.
#[derive(Debug, Clone, Copy)]
pub struct CoverViewer {
    /// The owner or a moderator: unapproved files count.
    pub manages: bool,
    pub include_nsfw: bool,
}

/// An album's cover: its chosen cover file if the viewer may see it, else
/// the first picture or video in the album's order that they may. Audio
/// has nothing to show, so it is never a cover.
pub async fn album_cover(
    db: &Db,
    album: AlbumId,
    chosen: Option<FileId>,
    viewer: CoverViewer,
    now: DateTime<Utc>,
) -> Result<Option<MediaFile>, sqlx::Error> {
    let mut sql: QueryBuilder<MySql> = QueryBuilder::new(format!(
        "SELECT {COLUMNS} FROM ih_files f WHERE f.album_id = "
    ));
    sql.push_bind(album.0)
        .push(" AND f.state = 'ready' AND f.deleted_at IS NULL AND f.kind <> 'audio'")
        .push(" AND (f.expires_at IS NULL OR f.expires_at > ")
        .push_bind(now)
        .push(")");
    if !viewer.manages {
        sql.push(" AND f.is_approved = 1");
    }
    if !viewer.include_nsfw {
        sql.push(" AND f.nsfw = 0");
    }
    sql.push(" ORDER BY f.id <=> ")
        .push_bind(chosen.map(|f| f.0))
        .push(" DESC, f.album_position, f.id LIMIT 1");
    let row: Option<FileRow> = sql.build_query_as().fetch_optional(db).await?;
    row.map(FileRow::into_domain).transpose()
}

/// The public id of the file imported with this Chevereto id, if any.
pub async fn id_encoded_by_legacy(db: &Db, legacy: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT id_encoded FROM ih_files WHERE legacy_id_encoded = ? LIMIT 1")
        .bind(legacy)
        .fetch_optional(db)
        .await
}

pub async fn increment_views(db: &Db, id: FileId) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE ih_files SET view_count = view_count + 1 WHERE id = ?")
        .bind(id.0)
        .execute(db)
        .await?;
    Ok(())
}

/// Like a file. Returns whether this call added the like; liking twice is a
/// no-op, not a second count.
pub async fn like(db: &Db, user: UserId, file: FileId) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let done = sqlx::query("INSERT IGNORE INTO ih_likes (user_id, file_id) VALUES (?, ?)")
        .bind(user.0)
        .bind(file.0)
        .execute(&mut *tx)
        .await?;
    let added = done.rows_affected() > 0;
    if added {
        sqlx::query("UPDATE ih_files SET like_count = like_count + 1 WHERE id = ?")
            .bind(file.0)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(added)
}

/// Remove a like. Returns whether there was one.
pub async fn unlike(db: &Db, user: UserId, file: FileId) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let done = sqlx::query("DELETE FROM ih_likes WHERE user_id = ? AND file_id = ?")
        .bind(user.0)
        .bind(file.0)
        .execute(&mut *tx)
        .await?;
    let removed = done.rows_affected() > 0;
    if removed {
        sqlx::query(
            "UPDATE ih_files SET like_count = GREATEST(CAST(like_count AS SIGNED) - 1, 0)
             WHERE id = ?",
        )
        .bind(file.0)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(removed)
}

/// The sort column and direction for each ordering. The id breaks ties in
/// the same direction, which is what makes the cursor exact.
const fn sort_spec(sort: FileSort) -> (&'static str, bool) {
    match sort {
        FileSort::Newest => ("f.created_at", false),
        FileSort::Oldest => ("f.created_at", true),
        FileSort::MostViewed => ("f.view_count", false),
        FileSort::MostLiked => ("f.like_count", false),
        FileSort::AlbumOrder => ("f.album_position", true),
    }
}

fn sort_key(file: &MediaFile, sort: FileSort) -> i64 {
    match sort {
        FileSort::Newest | FileSort::Oldest => file.created_at.timestamp(),
        FileSort::MostViewed => i64::try_from(file.view_count).unwrap_or(i64::MAX),
        FileSort::MostLiked => i64::from(file.like_count),
        FileSort::AlbumOrder => i64::from(file.album_position.unwrap_or(0)),
    }
}

/// One page of files.
pub async fn list(db: &Db, query: &FileQuery) -> Result<Page<MediaFile, FileSort>, DbError> {
    if query.sort == FileSort::AlbumOrder && !matches!(query.scope, FileScope::Album(_)) {
        return Err(DbError::Invalid {
            field: "sort",
            reason: "manual order exists only within an album",
        });
    }
    if let Some(cursor) = &query.after
        && cursor.sort != query.sort
    {
        return Err(DbError::Invalid {
            field: "cursor",
            reason: "not a cursor for this list",
        });
    }

    let mut sql: QueryBuilder<MySql> =
        QueryBuilder::new(format!("SELECT {COLUMNS} FROM ih_files f"));
    if query.visibility == Visibility::Listed {
        sql.push(
            " LEFT JOIN ih_albums a ON a.id = f.album_id
              LEFT JOIN ih_users u ON u.id = f.user_id",
        );
    }

    sql.push(" WHERE 1 = 1");
    match query.scope {
        FileScope::Everyone => {}
        FileScope::User(user) => {
            sql.push(" AND f.user_id = ").push_bind(user.0);
        }
        FileScope::Album(album) => {
            sql.push(" AND f.album_id = ").push_bind(album.0);
        }
        FileScope::Category(category) => {
            sql.push(" AND f.category_id = ").push_bind(category.0);
        }
        FileScope::Tag(tag) => {
            sql.push(" AND f.id IN (SELECT file_id FROM ih_file_tags WHERE tag_id = ")
                .push_bind(tag.0)
                .push(")");
        }
    }

    if query.visibility == Visibility::Deleted {
        sql.push(" AND f.deleted_at IS NOT NULL");
    } else {
        sql.push(" AND f.state = 'ready' AND f.deleted_at IS NULL")
            .push(" AND (f.expires_at IS NULL OR f.expires_at > ")
            .push_bind(query.now)
            .push(")");
    }
    if matches!(query.visibility, Visibility::Viewer | Visibility::Listed) {
        sql.push(" AND f.is_approved = 1");
    }
    if query.visibility == Visibility::Listed {
        sql.push(" AND (f.album_id IS NULL OR a.privacy = 'public')")
            .push(" AND (f.user_id IS NULL OR u.is_private = 0)");
    }
    if !query.include_nsfw {
        sql.push(" AND f.nsfw = 0");
    }
    if let Some(kind) = query.kind {
        sql.push(" AND f.kind = ").push_bind(kind.as_str());
    }

    let (column, ascending) = sort_spec(query.sort);
    let (cmp, dir) = if ascending {
        (">", "ASC")
    } else {
        ("<", "DESC")
    };
    if let Some(cursor) = &query.after {
        sql.push(format!(" AND ({column} {cmp} "));
        push_key(&mut sql, query.sort, cursor.key);
        sql.push(format!(" OR ({column} = "));
        push_key(&mut sql, query.sort, cursor.key);
        sql.push(format!(" AND f.id {cmp} "))
            .push_bind(cursor.id)
            .push("))");
    }
    sql.push(format!(" ORDER BY {column} {dir}, f.id {dir} LIMIT "))
        .push_bind(u64::from(query.limit) + 1);

    let rows: Vec<FileRow> = sql.build_query_as().fetch_all(db).await?;
    let mut items = rows
        .into_iter()
        .map(FileRow::into_domain)
        .collect::<Result<Vec<_>, _>>()?;

    let next = if items.len() > query.limit as usize {
        items.truncate(query.limit as usize);
        items
            .last()
            .map(|last| Cursor::new(query.sort, sort_key(last, query.sort), last.id.0))
    } else {
        None
    };
    Ok(Page { items, next })
}

/// Bind a cursor key in the column's own type: dates as datetimes, so the
/// comparison can use the index.
fn push_key(sql: &mut QueryBuilder<'_, MySql>, sort: FileSort, key: i64) {
    match sort {
        FileSort::Newest | FileSort::Oldest => {
            let at = Utc.timestamp_opt(key, 0).single().unwrap_or_default();
            sql.push_bind(at);
        }
        _ => {
            sql.push_bind(key);
        }
    }
}

/// Live files whose expiry has passed, oldest first, at most `limit` of
/// them. They are still undeleted rows: expiring one is a deletion like any
/// other, retained until an administrator purges it.
pub async fn expired(db: &Db, now: DateTime<Utc>, limit: u32) -> Result<Vec<FileId>, sqlx::Error> {
    let ids: Vec<u64> = sqlx::query_scalar(
        "SELECT id FROM ih_files
         WHERE expires_at IS NOT NULL AND expires_at <= ? AND deleted_at IS NULL
           AND state = 'ready'
         ORDER BY expires_at, id
         LIMIT ?",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(ids.into_iter().map(FileId).collect())
}
