//! The moderation queue.

use crate::Db;
use chrono::{DateTime, Utc};
use haloid_core::ids::{FileId, UserId};

/// Hold a file for review. A file already queued stays queued as it was.
pub async fn enqueue(db: &Db, file: FileId, reason: Option<&str>) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT IGNORE INTO ih_moderation_queue (file_id, reason) VALUES (?, ?)")
        .bind(file.0)
        .bind(reason)
        .execute(db)
        .await?;
    Ok(())
}

/// How many files are waiting for a decision.
pub async fn pending_count(db: &Db) -> Result<u64, sqlx::Error> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ih_moderation_queue WHERE decision = 'pending'")
            .fetch_one(db)
            .await?;
    Ok(u64::try_from(count).unwrap_or(0))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Queued {
    pub file_id: FileId,
    pub reason: Option<String>,
    pub queued_at: DateTime<Utc>,
}

/// Files waiting for a decision, oldest first, after `after` (a file id
/// from the previous page).
pub async fn pending(
    db: &Db,
    after: Option<FileId>,
    limit: u32,
) -> Result<Vec<Queued>, sqlx::Error> {
    let rows: Vec<(u64, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT q.file_id, q.reason, q.queued_at FROM ih_moderation_queue q
         JOIN ih_files f ON f.id = q.file_id
         WHERE q.decision = 'pending' AND f.deleted_at IS NULL AND q.file_id > ?
         ORDER BY q.file_id
         LIMIT ?",
    )
    .bind(after.map_or(0, |f| f.0))
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(file_id, reason, queued_at)| Queued {
            file_id: FileId(file_id),
            reason,
            queued_at,
        })
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approved,
    Denied,
}

/// Record a decision. Returns whether the file was waiting for one.
pub async fn decide(
    db: &Db,
    file: FileId,
    decision: Decision,
    by: Option<UserId>,
    note: Option<&str>,
    at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE ih_moderation_queue SET decision = ?, decided_by = ?, decided_at = ?, note = ?
         WHERE file_id = ? AND decision = 'pending'",
    )
    .bind(match decision {
        Decision::Approved => "approved",
        Decision::Denied => "denied",
    })
    .bind(by.map(|u| u.0))
    .bind(at)
    .bind(note)
    .bind(file.0)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}
