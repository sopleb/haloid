//! haloid-jobs — the background work, run inside the server process.
//!
//! One task set in the binary rather than cron: a self-hosted host should be
//! one systemd unit, and cron cannot read our settings. The queue is durable
//! in `ih_jobs` (see `haloid_db::jobs`), so a restart loses nothing, and
//! claiming skips locked rows, so a second process is safe.
//!
//! The scheduler enqueues the recurring jobs blindly on their period; a
//! job's kind is its dedupe key, so a tick while the last run is still
//! waiting adds nothing.

use chrono::{DateTime, Utc};
use haloid_db::jobs::{self, Job, NewJob};
use haloid_service::app::App;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// The job kinds, as stored in `ih_jobs.kind`.
pub mod kinds {
    /// Soft-delete files whose expiry has passed.
    pub const EXPIRE_FILES: &str = "expire_files";
    /// Remove expired sessions.
    pub const EXPIRE_SESSIONS: &str = "expire_sessions";
    /// Drop the chunks of chunked uploads left unfinished.
    pub const EXPIRE_UPLOAD_SESSIONS: &str = "expire_upload_sessions";
    /// Destroy one file, row and bytes. Payload `{"file_id": <row id>}`.
    /// Queued only by an administrator's purge.
    pub const PURGE_FILE: &str = haloid_service::admin::PURGE_FILE_JOB;
}

/// How often each recurring job is enqueued.
pub const SCHEDULE: &[(&str, Duration)] = &[
    (kinds::EXPIRE_FILES, Duration::from_secs(60)),
    (kinds::EXPIRE_SESSIONS, Duration::from_secs(3600)),
    (kinds::EXPIRE_UPLOAD_SESSIONS, Duration::from_secs(3600)),
];

/// A worker that took a job this long ago and never finished it has died.
pub const STALE_AFTER: Duration = Duration::from_secs(15 * 60);
/// Finished jobs are kept this long, for anyone looking at what ran.
pub const KEEP_DONE: Duration = Duration::from_secs(7 * 24 * 3600);
/// Files expired per batch, so one run never holds the database long.
const EXPIRE_BATCH: u32 = 100;
const EXPIRE_BATCHES_PER_RUN: usize = 20;

/// The wait before retry `attempt` (counting from 1): ten seconds, doubling,
/// at most an hour.
pub fn backoff(attempt: u32) -> Duration {
    let secs = 10u64.saturating_mul(1u64 << attempt.saturating_sub(1).min(16));
    Duration::from_secs(secs.min(3600))
}

fn later(now: DateTime<Utc>, by: Duration) -> DateTime<Utc> {
    now + chrono::Duration::from_std(by).unwrap_or(chrono::Duration::MAX)
}

/// Queue a recurring job now, unless one is already waiting.
pub async fn enqueue_recurring(
    app: &App,
    kind: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    jobs::enqueue(
        &app.db,
        &NewJob {
            kind,
            payload: serde_json::json!({}),
            dedupe_key: Some(kind),
            run_at: now,
            max_attempts: 5,
        },
    )
    .await
    .map(|_| ())
}

/// Queue the destruction of a file. Only for an administrator's purge.
pub async fn enqueue_purge(app: &App, file_id: u64, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let key = format!("{}:{file_id}", kinds::PURGE_FILE);
    jobs::enqueue(
        &app.db,
        &NewJob {
            kind: kinds::PURGE_FILE,
            payload: serde_json::json!({ "file_id": file_id }),
            dedupe_key: Some(&key),
            run_at: now,
            max_attempts: 10,
        },
    )
    .await
    .map(|_| ())
}

/// Do one job's work.
async fn execute(app: &App, job: &Job, now: DateTime<Utc>) -> Result<(), String> {
    match job.kind.as_str() {
        kinds::EXPIRE_FILES => {
            let mut failures = Vec::new();
            for _ in 0..EXPIRE_BATCHES_PER_RUN {
                let due = haloid_db::files::expired(&app.db, now, EXPIRE_BATCH)
                    .await
                    .map_err(|e| e.to_string())?;
                let full = due.len() == EXPIRE_BATCH as usize;
                for id in due {
                    if let Err(error) = app.files.expire(id, now).await {
                        failures.push(format!("file {}: {error}", id.0));
                    }
                }
                // A file that keeps failing stays due; stop rather than
                // spin on it, and let the retry come back to it.
                if !full || !failures.is_empty() {
                    break;
                }
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err(failures.join("; "))
            }
        }
        kinds::EXPIRE_SESSIONS => {
            let gone = haloid_db::sessions::delete_expired(&app.db, now)
                .await
                .map_err(|e| e.to_string())?;
            if gone > 0 {
                tracing::debug!(gone, "expired sessions removed");
            }
            Ok(())
        }
        kinds::EXPIRE_UPLOAD_SESSIONS => {
            let gone = app.chunked.expire(now).await.map_err(|e| e.to_string())?;
            if gone > 0 {
                tracing::debug!(gone, "abandoned upload sessions removed");
            }
            Ok(())
        }
        kinds::PURGE_FILE => {
            let id = job
                .payload
                .get("file_id")
                .and_then(serde_json::Value::as_u64)
                .ok_or("payload has no file_id")?;
            app.files
                .purge_queued(haloid_core::ids::FileId(id))
                .await
                .map_err(|e| e.to_string())
        }
        other => Err(format!("unknown job kind {other:?}")),
    }
}

pub struct Runner {
    app: Arc<App>,
    worker: String,
}

impl Runner {
    pub fn new(app: Arc<App>) -> Self {
        Self {
            app,
            worker: format!("haloid-{}", std::process::id()),
        }
    }

    /// Claim and run one due job. Returns whether there was one.
    pub async fn run_once(&self, now: DateTime<Utc>) -> Result<bool, sqlx::Error> {
        let Some(job) = jobs::claim(&self.app.db, &self.worker, now).await? else {
            return Ok(false);
        };
        let outcome = execute(&self.app, &job, now).await;
        self.app.metrics.job(outcome.is_ok());
        match outcome {
            Ok(()) => jobs::complete(&self.app.db, job.id, Utc::now()).await?,
            Err(error) => {
                let retry = (job.attempts < job.max_attempts)
                    .then(|| later(Utc::now(), backoff(job.attempts)));
                if retry.is_some() {
                    tracing::warn!(id = job.id, kind = %job.kind, attempt = job.attempts, %error, "job failed; will retry");
                } else {
                    tracing::error!(id = job.id, kind = %job.kind, attempt = job.attempts, %error, "job failed for good");
                }
                jobs::fail(&self.app.db, job.id, &error, retry, Utc::now()).await?;
            }
        }
        Ok(true)
    }

    /// Hand back abandoned jobs and forget old finished ones.
    pub async fn housekeep(&self, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let stale = jobs::reset_stale(
            &self.app.db,
            now - chrono::Duration::from_std(STALE_AFTER).unwrap_or_default(),
        )
        .await?;
        if stale > 0 {
            tracing::warn!(stale, "reclaimed jobs abandoned by a dead worker");
        }
        jobs::prune_done(
            &self.app.db,
            now - chrono::Duration::from_std(KEEP_DONE).unwrap_or_default(),
        )
        .await?;
        Ok(())
    }

    /// Run until `shutdown` turns true: the scheduler, housekeeping, and the
    /// worker loop, which sleeps a second when there is nothing to do.
    pub fn spawn(self, mut shutdown: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut last_enqueued: Vec<Option<std::time::Instant>> = vec![None; SCHEDULE.len()];
            let mut last_housekeeping: Option<std::time::Instant> = None;
            loop {
                if *shutdown.borrow() {
                    break;
                }
                let now = Utc::now();
                for (slot, (kind, every)) in last_enqueued.iter_mut().zip(SCHEDULE) {
                    if slot.is_none_or(|at| at.elapsed() >= *every) {
                        match enqueue_recurring(&self.app, kind, now).await {
                            Ok(()) => *slot = Some(std::time::Instant::now()),
                            Err(error) => tracing::warn!(%error, kind, "could not schedule a job"),
                        }
                    }
                }
                if last_housekeeping.is_none_or(|at| at.elapsed() >= Duration::from_secs(300)) {
                    if let Err(error) = self.housekeep(now).await {
                        tracing::warn!(%error, "job housekeeping failed");
                    }
                    last_housekeeping = Some(std::time::Instant::now());
                }

                let worked = match self.run_once(now).await {
                    Ok(worked) => worked,
                    Err(error) => {
                        tracing::warn!(%error, "the job queue is unreachable");
                        false
                    }
                };
                if !worked {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(1)) => {}
                        _ = shutdown.changed() => {}
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_ten_seconds_and_stops_at_an_hour() {
        assert_eq!(backoff(1), Duration::from_secs(10));
        assert_eq!(backoff(2), Duration::from_secs(20));
        assert_eq!(backoff(4), Duration::from_secs(80));
        assert_eq!(backoff(9), Duration::from_secs(2560));
        assert_eq!(backoff(10), Duration::from_secs(3600));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(3600));
    }
}
