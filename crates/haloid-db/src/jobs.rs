//! The durable job queue in `ih_jobs`.
//!
//! Workers claim with `FOR UPDATE SKIP LOCKED`, so any number of them — in
//! one process or several — never take the same job twice. A job that fails
//! goes back to `pending` with a later `run_at` until it runs out of
//! attempts; a worker that dies mid-job leaves it `running`, and
//! [`reset_stale`] hands it back.
//!
//! `dedupe_key` lets the scheduler enqueue blindly: while a job with that key
//! is pending or running, enqueueing another is a no-op. The key is released
//! when the job finishes, either way, so the next tick can enqueue afresh.

use crate::Db;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: u64,
    pub kind: String,
    pub payload: serde_json::Value,
    /// Counting the run that has just been claimed.
    pub attempts: u32,
    pub max_attempts: u32,
}

#[derive(Debug, Clone)]
pub struct NewJob<'a> {
    pub kind: &'a str,
    pub payload: serde_json::Value,
    pub dedupe_key: Option<&'a str>,
    pub run_at: DateTime<Utc>,
    pub max_attempts: u32,
}

#[derive(sqlx::FromRow)]
struct JobRow {
    id: u64,
    kind: String,
    payload: sqlx::types::Json<serde_json::Value>,
    attempts: u32,
    max_attempts: u32,
}

/// Queue a job. Returns its id, or `None` when a job with the same
/// `dedupe_key` is already waiting or running.
pub async fn enqueue(db: &Db, job: &NewJob<'_>) -> Result<Option<u64>, sqlx::Error> {
    let done = sqlx::query(
        "INSERT IGNORE INTO ih_jobs (kind, payload, dedupe_key, run_at, max_attempts)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(job.kind)
    .bind(sqlx::types::Json(&job.payload))
    .bind(job.dedupe_key)
    .bind(job.run_at)
    .bind(job.max_attempts)
    .execute(db)
    .await?;
    Ok((done.rows_affected() > 0).then(|| done.last_insert_id()))
}

/// Take the next job that is due, marking it running under `worker`.
pub async fn claim(db: &Db, worker: &str, now: DateTime<Utc>) -> Result<Option<Job>, sqlx::Error> {
    let mut tx = db.begin().await?;
    let row: Option<JobRow> = sqlx::query_as(
        "SELECT id, kind, payload, attempts, max_attempts FROM ih_jobs
         WHERE status = 'pending' AND run_at <= ?
         ORDER BY run_at, id
         LIMIT 1
         FOR UPDATE SKIP LOCKED",
    )
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    sqlx::query(
        "UPDATE ih_jobs SET status = 'running', attempts = attempts + 1,
           locked_at = ?, locked_by = ?
         WHERE id = ?",
    )
    .bind(now)
    .bind(worker)
    .bind(row.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(Job {
        id: row.id,
        kind: row.kind,
        payload: row.payload.0,
        attempts: row.attempts + 1,
        max_attempts: row.max_attempts,
    }))
}

/// The job succeeded.
pub async fn complete(db: &Db, id: u64, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ih_jobs SET status = 'done', finished_at = ?, dedupe_key = NULL,
           locked_at = NULL, locked_by = NULL, last_error = NULL
         WHERE id = ?",
    )
    .bind(now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// The job failed. With `retry_at` it waits to run again; without, it is
/// finished as `failed` and keeps its error for an administrator to read.
pub async fn fail(
    db: &Db,
    id: u64,
    error: &str,
    retry_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    match retry_at {
        Some(at) => {
            sqlx::query(
                "UPDATE ih_jobs SET status = 'pending', run_at = ?, last_error = ?,
                   locked_at = NULL, locked_by = NULL
                 WHERE id = ?",
            )
            .bind(at)
            .bind(error)
            .bind(id)
            .execute(db)
            .await?
        }
        None => {
            sqlx::query(
                "UPDATE ih_jobs SET status = 'failed', finished_at = ?, last_error = ?,
                   dedupe_key = NULL, locked_at = NULL, locked_by = NULL
                 WHERE id = ?",
            )
            .bind(now)
            .bind(error)
            .bind(id)
            .execute(db)
            .await?
        }
    };
    Ok(())
}

/// Hand back jobs whose worker took them before `cutoff` and never
/// finished: a crash, a kill, a lost connection. Returns how many.
pub async fn reset_stale(db: &Db, cutoff: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE ih_jobs SET status = 'pending', locked_at = NULL, locked_by = NULL
         WHERE status = 'running' AND locked_at < ?",
    )
    .bind(cutoff)
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

/// Forget successful jobs finished before `cutoff`. Failed ones stay until
/// someone looks at them.
pub async fn prune_done(db: &Db, cutoff: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    let done = sqlx::query("DELETE FROM ih_jobs WHERE status = 'done' AND finished_at < ?")
        .bind(cutoff)
        .execute(db)
        .await?;
    Ok(done.rows_affected())
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct JobState {
    pub status: String,
    pub attempts: u32,
    pub last_error: Option<String>,
}

/// One job's state, for tests and the admin view.
pub async fn state(db: &Db, id: u64) -> Result<Option<JobState>, sqlx::Error> {
    sqlx::query_as("SELECT status, attempts, last_error FROM ih_jobs WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Jobs waiting to run, for the metrics.
pub async fn pending_count(db: &Db) -> Result<u64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ih_jobs WHERE status = 'pending'")
        .fetch_one(db)
        .await?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Jobs that ran out of attempts, for the metrics: each one wants a look.
pub async fn failed_count(db: &Db) -> Result<u64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ih_jobs WHERE status = 'failed'")
        .fetch_one(db)
        .await?;
    Ok(u64::try_from(count).unwrap_or(0))
}
