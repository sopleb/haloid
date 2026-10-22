//! The albums repository.
//!
//! Albums nest through `parent_id`, at most [`MAX_ALBUM_DEPTH`] deep, and
//! never in a cycle; both rules are enforced here because the schema cannot
//! express them. A parent must belong to the same owner.
//!
//! Deleting an album is one of two operations, and the caller says which:
//! keep its files (they become unfiled, its sub-albums move up a level) or
//! take its contents with it (every album in the sub-tree goes, and their
//! files are deleted — retained for audit, like any other file deletion).

use crate::error::{DbError, unique_violation};
use crate::files::soft_delete_in;
use crate::{Db, decode_enum};
use chrono::{DateTime, Utc};
use haloid_core::files::{Album, AlbumPrivacy, DeleteReason, Deletion, MAX_ALBUM_DEPTH};
use haloid_core::ids::{AlbumId, FileId, UserId};
use haloid_core::pagination::{AlbumSort, Cursor, Page};
use sqlx::MySqlConnection;

#[derive(Debug, Clone)]
pub struct NewAlbum<'a> {
    pub user_id: Option<UserId>,
    pub parent_id: Option<AlbumId>,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub privacy: AlbumPrivacy,
    /// Argon2id PHC string, for `AlbumPrivacy::Password`.
    pub password_hash: Option<&'a str>,
}

/// The editable fields, replaced wholesale.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumDetails {
    pub name: String,
    pub description: Option<String>,
    pub privacy: AlbumPrivacy,
    pub password_hash: Option<String>,
}

/// What deleting an album does to what is in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumDeletion {
    /// The album goes. Its files stay in their owner's library, unfiled; its
    /// sub-albums move up to its parent.
    KeepContents,
    /// The album and its whole sub-tree go, and every file in them is
    /// deleted with [`DeleteReason::Album`] — retained until purged.
    WithContents {
        by: Option<UserId>,
        at: DateTime<Utc>,
    },
}

const COLUMNS: &str = "a.id, a.id_encoded, a.user_id, a.parent_id, a.name, a.description, \
     a.privacy, (a.password_hash IS NOT NULL) AS has_password, a.cover_file_id, a.file_count, \
     a.view_count, a.created_at, a.updated_at";

#[derive(sqlx::FromRow)]
struct AlbumRow {
    id: u64,
    id_encoded: String,
    user_id: Option<u64>,
    parent_id: Option<u64>,
    name: String,
    description: Option<String>,
    privacy: String,
    has_password: bool,
    cover_file_id: Option<u64>,
    file_count: u32,
    view_count: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl AlbumRow {
    fn into_domain(self) -> Result<Album, sqlx::Error> {
        Ok(Album {
            id: AlbumId(self.id),
            id_encoded: self.id_encoded.into(),
            user_id: self.user_id.map(UserId),
            parent_id: self.parent_id.map(AlbumId),
            name: self.name,
            description: self.description,
            privacy: decode_enum("privacy", &self.privacy)?,
            has_password: self.has_password,
            cover_file_id: self.cover_file_id.map(FileId),
            file_count: self.file_count,
            view_count: self.view_count,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// Recursion guard for the tree queries. Well above [`MAX_ALBUM_DEPTH`], so
/// it only matters if a cycle was ever written behind this module's back.
const WALK_LIMIT: u32 = 64;

/// How deep `album` sits: a root album is depth 1. `None` if it is missing.
async fn depth(conn: &mut MySqlConnection, album: AlbumId) -> Result<Option<u32>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<i64>>(
        "WITH RECURSIVE up (id, parent_id, depth) AS (
           SELECT id, parent_id, 1 FROM ih_albums WHERE id = ?
           UNION ALL
           SELECT a.id, a.parent_id, up.depth + 1
           FROM ih_albums a JOIN up ON a.id = up.parent_id
           WHERE up.depth < ?
         )
         SELECT MAX(depth) FROM up",
    )
    .bind(album.0)
    .bind(WALK_LIMIT)
    .fetch_one(conn)
    .await
    .map(|d| d.and_then(|d| u32::try_from(d).ok()))
}

/// `album` and everything below it, with each one's level relative to it
/// (the album itself is 1).
async fn subtree(
    conn: &mut MySqlConnection,
    album: AlbumId,
) -> Result<Vec<(u64, u32)>, sqlx::Error> {
    let rows: Vec<(u64, i64)> = sqlx::query_as(
        "WITH RECURSIVE down (id, level) AS (
           SELECT id, 1 FROM ih_albums WHERE id = ?
           UNION ALL
           SELECT a.id, down.level + 1
           FROM ih_albums a JOIN down ON a.parent_id = down.id
           WHERE down.level < ?
         )
         SELECT id, level FROM down",
    )
    .bind(album.0)
    .bind(WALK_LIMIT)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, level)| (id, u32::try_from(level).unwrap_or(u32::MAX)))
        .collect())
}

/// Check that `parent` exists, belongs to `owner`, and has room for a
/// sub-tree `height` levels tall beneath it.
async fn check_parent(
    conn: &mut MySqlConnection,
    parent: AlbumId,
    owner: Option<UserId>,
    height: u32,
) -> Result<(), DbError> {
    let parent_owner: Option<Option<u64>> =
        sqlx::query_scalar("SELECT user_id FROM ih_albums WHERE id = ?")
            .bind(parent.0)
            .fetch_optional(&mut *conn)
            .await?;
    match parent_owner {
        None => {
            return Err(DbError::Invalid {
                field: "parent_id",
                reason: "no such album",
            });
        }
        Some(parent_owner) if parent_owner != owner.map(|u| u.0) => {
            return Err(DbError::Invalid {
                field: "parent_id",
                reason: "belongs to another user",
            });
        }
        Some(_) => {}
    }
    let parent_depth = depth(conn, parent).await?.unwrap_or(0);
    if parent_depth + height > MAX_ALBUM_DEPTH {
        return Err(DbError::Invalid {
            field: "parent_id",
            reason: "albums nest at most 8 deep",
        });
    }
    Ok(())
}

const ID_ATTEMPTS: usize = 3;

pub async fn create(db: &Db, new: &NewAlbum<'_>) -> Result<Album, DbError> {
    let mut tx = db.begin().await?;
    if let Some(parent) = new.parent_id {
        check_parent(&mut tx, parent, new.user_id, 1).await?;
    }

    for _ in 0..ID_ATTEMPTS {
        let result = sqlx::query(
            "INSERT INTO ih_albums
               (id_encoded, user_id, parent_id, name, description, privacy, password_hash)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(haloid_id::generate_public_id())
        .bind(new.user_id.map(|id| id.0))
        .bind(new.parent_id.map(|id| id.0))
        .bind(new.name)
        .bind(new.description)
        .bind(new.privacy.as_str())
        .bind(new.password_hash)
        .execute(&mut *tx)
        .await;

        match result {
            Ok(done) => {
                if let Some(user) = new.user_id {
                    sqlx::query("UPDATE ih_users SET album_count = album_count + 1 WHERE id = ?")
                        .bind(user.0)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
                return find_by_id(db, AlbumId(done.last_insert_id()))
                    .await?
                    .ok_or(DbError::Sqlx(sqlx::Error::RowNotFound));
            }
            Err(e) if unique_violation(&e).as_deref() == Some("uq_albums_id_encoded") => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(DbError::Conflict("id_encoded"))
}

pub async fn find_by_id(db: &Db, id: AlbumId) -> Result<Option<Album>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_albums a WHERE a.id = ?");
    sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(id.0)
        .fetch_optional(db)
        .await?
        .map(AlbumRow::into_domain)
        .transpose()
}

pub async fn find_by_id_encoded(db: &Db, id_encoded: &str) -> Result<Option<Album>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_albums a WHERE a.id_encoded = ?");
    sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(id_encoded)
        .fetch_optional(db)
        .await?
        .map(AlbumRow::into_domain)
        .transpose()
}

/// The password hash of a password-protected album, for the service to
/// verify against.
pub async fn password_hash(db: &Db, id: AlbumId) -> Result<Option<String>, sqlx::Error> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>("SELECT password_hash FROM ih_albums WHERE id = ?")
            .bind(id.0)
            .fetch_optional(db)
            .await?
            .flatten(),
    )
}

/// One page of a user's albums directly under `parent` (`None`: the
/// top-level ones), newest first. `listed_only` restricts it to public
/// albums, for someone else's profile.
pub async fn list(
    db: &Db,
    owner: UserId,
    parent: Option<AlbumId>,
    listed_only: bool,
    after: Option<Cursor<AlbumSort>>,
    limit: u32,
) -> Result<Page<Album, AlbumSort>, sqlx::Error> {
    let mut sql = format!(
        "SELECT {COLUMNS} FROM ih_albums a
         WHERE a.user_id = ? AND a.parent_id <=> ?"
    );
    if listed_only {
        sql.push_str(" AND a.privacy = 'public'");
    }
    if after.is_some() {
        sql.push_str(" AND (a.created_at < ? OR (a.created_at = ? AND a.id < ?))");
    }
    sql.push_str(" ORDER BY a.created_at DESC, a.id DESC LIMIT ?");

    let mut query = sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(owner.0)
        .bind(parent.map(|p| p.0));
    if let Some(cursor) = after {
        let at = DateTime::from_timestamp(cursor.key, 0).unwrap_or_default();
        query = query.bind(at).bind(at).bind(cursor.id);
    }
    let rows = query.bind(u64::from(limit) + 1).fetch_all(db).await?;

    let mut items = rows
        .into_iter()
        .map(AlbumRow::into_domain)
        .collect::<Result<Vec<_>, _>>()?;
    let next = if items.len() > limit as usize {
        items.truncate(limit as usize);
        items
            .last()
            .map(|a| Cursor::new(AlbumSort::Newest, a.created_at.timestamp(), a.id.0))
    } else {
        None
    };
    Ok(Page { items, next })
}

/// Replace the editable fields. Returns whether the album exists.
pub async fn update(db: &Db, id: AlbumId, details: &AlbumDetails) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE ih_albums SET name = ?, description = ?, privacy = ?, password_hash = ?
         WHERE id = ?",
    )
    .bind(&details.name)
    .bind(&details.description)
    .bind(details.privacy.as_str())
    .bind(&details.password_hash)
    .bind(id.0)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The public id of the album imported with this Chevereto id, if any.
pub async fn id_encoded_by_legacy(db: &Db, legacy: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT id_encoded FROM ih_albums WHERE legacy_id_encoded = ? LIMIT 1")
        .bind(legacy)
        .fetch_optional(db)
        .await
}

/// Choose the album's cover, or clear it. Returns whether the album exists.
pub async fn set_cover(db: &Db, id: AlbumId, cover: Option<FileId>) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("UPDATE ih_albums SET cover_file_id = ? WHERE id = ?")
        .bind(cover.map(|f| f.0))
        .bind(id.0)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Move an album, with everything under it, beneath `parent` (or to the top
/// level). Refuses a move into its own sub-tree and one that would nest the
/// tree too deep.
pub async fn set_parent(db: &Db, id: AlbumId, parent: Option<AlbumId>) -> Result<(), DbError> {
    let mut tx = db.begin().await?;
    let owner: Option<Option<u64>> =
        sqlx::query_scalar("SELECT user_id FROM ih_albums WHERE id = ? FOR UPDATE")
            .bind(id.0)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(owner) = owner else {
        return Err(DbError::Sqlx(sqlx::Error::RowNotFound));
    };

    if let Some(parent) = parent {
        let below = subtree(&mut tx, id).await?;
        if below.iter().any(|(album, _)| *album == parent.0) {
            return Err(DbError::Invalid {
                field: "parent_id",
                reason: "an album cannot move inside itself",
            });
        }
        let height = below.iter().map(|(_, level)| *level).max().unwrap_or(1);
        check_parent(&mut tx, parent, owner.map(UserId), height).await?;
    }

    sqlx::query("UPDATE ih_albums SET parent_id = ? WHERE id = ?")
        .bind(parent.map(|p| p.0))
        .bind(id.0)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Delete an album; see [`AlbumDeletion`] for what happens to its contents.
/// Returns `None` if there was no such album, else the files it deleted
/// along with it — whose bytes the caller still has to quarantine.
pub async fn delete(
    db: &Db,
    id: AlbumId,
    mode: AlbumDeletion,
) -> Result<Option<Vec<FileId>>, sqlx::Error> {
    let mut tx = db.begin().await?;
    let album: Option<(Option<u64>, Option<u64>)> =
        sqlx::query_as("SELECT user_id, parent_id FROM ih_albums WHERE id = ? FOR UPDATE")
            .bind(id.0)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((owner, parent)) = album else {
        return Ok(None);
    };
    let mut deleted_files = Vec::new();

    let doomed: Vec<u64> = match mode {
        AlbumDeletion::KeepContents => {
            sqlx::query("UPDATE ih_albums SET parent_id = ? WHERE parent_id = ?")
                .bind(parent)
                .bind(id.0)
                .execute(&mut *tx)
                .await?;
            vec![id.0]
        }
        AlbumDeletion::WithContents { by, at } => {
            let albums: Vec<u64> = subtree(&mut tx, id)
                .await?
                .into_iter()
                .map(|(album, _)| album)
                .collect();
            let deletion = Deletion {
                at,
                by,
                reason: DeleteReason::Album,
            };
            for album in &albums {
                let files: Vec<u64> = sqlx::query_scalar(
                    "SELECT id FROM ih_files WHERE album_id = ? AND deleted_at IS NULL",
                )
                .bind(album)
                .fetch_all(&mut *tx)
                .await?;
                for file in files {
                    soft_delete_in(&mut tx, FileId(file), &deletion).await?;
                    deleted_files.push(FileId(file));
                }
            }
            albums
        }
    };

    for album in &doomed {
        // The foreign key would clear album_id on its own; the manual-order
        // position has to go with it.
        sqlx::query(
            "UPDATE ih_files SET album_id = NULL, album_position = NULL WHERE album_id = ?",
        )
        .bind(album)
        .execute(&mut *tx)
        .await?;
    }
    // Order does not matter: deleting a parent first sets its children's
    // parent_id to NULL (the foreign key's ON DELETE SET NULL), and they are
    // deleted in turn.
    for album in &doomed {
        sqlx::query("DELETE FROM ih_albums WHERE id = ?")
            .bind(album)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(owner) = owner {
        sqlx::query(
            "UPDATE ih_users
             SET album_count = GREATEST(CAST(album_count AS SIGNED) - ?, 0)
             WHERE id = ?",
        )
        .bind(doomed.len() as i64)
        .bind(owner)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Some(deleted_files))
}
