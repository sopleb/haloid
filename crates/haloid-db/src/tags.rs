//! The tags repository.
//!
//! A tag is identified by its slug (`haloid_core::taxonomy::tag_slug`), so
//! every spelling of "Cats" is one tag, named by whoever typed it first. Its
//! `file_count` counts live files, kept in step by the files repository as
//! files are published, deleted, restored and purged.

use crate::Db;
use haloid_core::ids::{FileId, TagId};
use haloid_core::taxonomy::{MAX_TAG_LEN, Tag, tag_slug};

const COLUMNS: &str = "t.id, t.name, t.slug, t.file_count";

#[derive(sqlx::FromRow)]
struct TagRow {
    id: u64,
    name: String,
    slug: String,
    file_count: u32,
}

impl From<TagRow> for Tag {
    fn from(row: TagRow) -> Self {
        Tag {
            id: TagId(row.id),
            name: row.name,
            slug: row.slug,
            file_count: row.file_count,
        }
    }
}

/// Replace a file's tags with `names`, creating tags as needed. Names that
/// share a slug are one tag; names with no usable characters are dropped.
/// Returns the file's tags afterwards, by slug.
///
/// How many tags a file may carry is the service's rule, not this one's.
pub async fn set_for_file(db: &Db, file: FileId, names: &[&str]) -> Result<Vec<Tag>, sqlx::Error> {
    let mut tx = db.begin().await?;
    let live: bool = sqlx::query_scalar(
        "SELECT (state = 'ready' AND deleted_at IS NULL) FROM ih_files WHERE id = ? FOR UPDATE",
    )
    .bind(file.0)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;

    // In the order written; the first spelling of each slug wins.
    let mut wanted: Vec<(String, String)> = Vec::new();
    for name in names {
        if let Some(slug) = tag_slug(name)
            && !wanted.iter().any(|(s, _)| *s == slug)
        {
            let name: String = name.trim().chars().take(MAX_TAG_LEN).collect();
            wanted.push((slug, name));
        }
    }

    let mut new_ids = Vec::with_capacity(wanted.len());
    for (slug, name) in &wanted {
        // LAST_INSERT_ID(id) makes an existing row's id the insert id, so
        // one statement both creates and finds.
        let done = sqlx::query(
            "INSERT INTO ih_tags (name, slug) VALUES (?, ?)
             ON DUPLICATE KEY UPDATE id = LAST_INSERT_ID(id)",
        )
        .bind(name)
        .bind(slug)
        .execute(&mut *tx)
        .await?;
        new_ids.push(done.last_insert_id());
    }

    let old_ids: Vec<u64> = sqlx::query_scalar("SELECT tag_id FROM ih_file_tags WHERE file_id = ?")
        .bind(file.0)
        .fetch_all(&mut *tx)
        .await?;

    for removed in old_ids.iter().filter(|id| !new_ids.contains(id)) {
        sqlx::query("DELETE FROM ih_file_tags WHERE file_id = ? AND tag_id = ?")
            .bind(file.0)
            .bind(removed)
            .execute(&mut *tx)
            .await?;
        if live {
            count(&mut tx, *removed, -1).await?;
        }
    }
    for added in new_ids.iter().filter(|id| !old_ids.contains(id)) {
        sqlx::query("INSERT INTO ih_file_tags (file_id, tag_id) VALUES (?, ?)")
            .bind(file.0)
            .bind(added)
            .execute(&mut *tx)
            .await?;
        if live {
            count(&mut tx, *added, 1).await?;
        }
    }
    for (position, id) in new_ids.iter().enumerate() {
        sqlx::query("UPDATE ih_file_tags SET position = ? WHERE file_id = ? AND tag_id = ?")
            .bind(u16::try_from(position).unwrap_or(u16::MAX))
            .bind(file.0)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    for_file(db, file).await
}

async fn count(conn: &mut sqlx::MySqlConnection, tag: u64, delta: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ih_tags SET file_count = GREATEST(CAST(file_count AS SIGNED) + ?, 0) WHERE id = ?",
    )
    .bind(delta)
    .bind(tag)
    .execute(conn)
    .await?;
    Ok(())
}

/// A file's tags, in the order they were written.
pub async fn for_file(db: &Db, file: FileId) -> Result<Vec<Tag>, sqlx::Error> {
    let sql = format!(
        "SELECT {COLUMNS} FROM ih_tags t JOIN ih_file_tags ft ON ft.tag_id = t.id
         WHERE ft.file_id = ? ORDER BY ft.position, t.slug"
    );
    let rows: Vec<TagRow> = sqlx::query_as(&sql).bind(file.0).fetch_all(db).await?;
    Ok(rows.into_iter().map(Tag::from).collect())
}

pub async fn find_by_slug(db: &Db, slug: &str) -> Result<Option<Tag>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_tags t WHERE t.slug = ?");
    Ok(sqlx::query_as::<_, TagRow>(&sql)
        .bind(slug)
        .fetch_optional(db)
        .await?
        .map(Tag::from))
}

/// The most used tags that are in use at all.
pub async fn popular(db: &Db, limit: u32) -> Result<Vec<Tag>, sqlx::Error> {
    let sql = format!(
        "SELECT {COLUMNS} FROM ih_tags t WHERE t.file_count > 0
         ORDER BY t.file_count DESC, t.slug LIMIT ?"
    );
    let rows: Vec<TagRow> = sqlx::query_as(&sql).bind(limit).fetch_all(db).await?;
    Ok(rows.into_iter().map(Tag::from).collect())
}
