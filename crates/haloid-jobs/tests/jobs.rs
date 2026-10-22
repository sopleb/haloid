//! The job queue and the first jobs, against a real MariaDB.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use chrono::{Duration, Utc};
use haloid_core::auth::Actor;
use haloid_core::files::DeleteReason;
use haloid_db::jobs::{self, NewJob};
use haloid_jobs::{Runner, enqueue_purge, enqueue_recurring, kinds};
use haloid_service::app::{App, AppConfig};
use haloid_service::upload::{UploadCommand, UploadSource};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

async fn app(db: &haloid_db::Db, dir: &TempDir) -> Arc<App> {
    Arc::new(
        App::start_default(
            db.clone(),
            AppConfig {
                data_dir: dir.path().to_path_buf(),
                altcha_secret: "test".into(),
                pinned_settings: BTreeMap::new(),
                pro: haloid_service::app::ProConfig::default(),
                altcha: None,
                updates: None,
            },
        )
        .await
        .unwrap(),
    )
}

async fn upload(app: &App) -> haloid_core::files::MediaFile {
    let bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/still.png"))
            .unwrap();
    app.uploads
        .upload(UploadCommand {
            actor: Actor::Guest,
            source: UploadSource::Stream {
                reader: Box::new(std::io::Cursor::new(bytes)),
                filename: Some("still.png".into()),
            },
            title: None,
            description: None,
            tags: Vec::new(),
            album: None,
            category: None,
            nsfw: None,
            expiration: Some(std::time::Duration::from_secs(3600)),
            width: None,
            uploader_ip: None,
        })
        .await
        .unwrap()
        .file
}

fn job<'a>(kind: &'a str, key: Option<&'a str>) -> NewJob<'a> {
    NewJob {
        kind,
        payload: serde_json::json!({}),
        dedupe_key: key,
        // Well in the past: tests capture `now` once, and a slow database
        // can make a later enqueue's own clock overtake it.
        run_at: Utc::now() - Duration::hours(1),
        max_attempts: 2,
    }
}

#[tokio::test]
async fn jobs_are_claimed_once_retried_and_given_up_on() {
    let scratch = migrated_or_skip!();
    let db = &scratch.pool;
    let now = Utc::now();

    let first = jobs::enqueue(db, &job("a", Some("only-one")))
        .await
        .unwrap();
    assert!(first.is_some());
    assert_eq!(
        jobs::enqueue(db, &job("a", Some("only-one")))
            .await
            .unwrap(),
        None,
        "a pending job with the same key makes a second a no-op"
    );
    let other = jobs::enqueue(db, &job("b", None)).await.unwrap().unwrap();

    // Two workers at once never take the same job.
    let (x, y) = tokio::join!(jobs::claim(db, "w1", now), jobs::claim(db, "w2", now));
    let (x, y) = (x.unwrap().unwrap(), y.unwrap().unwrap());
    assert_ne!(x.id, y.id);
    assert_eq!(
        jobs::claim(db, "w3", now).await.unwrap(),
        None,
        "nothing left"
    );
    let (keyed, unkeyed) = if x.id == other { (y, x) } else { (x, y) };
    assert_eq!(keyed.attempts, 1);
    jobs::complete(db, unkeyed.id, now).await.unwrap();

    // A failure with a retry waits for its time.
    jobs::fail(db, keyed.id, "boom", Some(now + Duration::seconds(30)), now)
        .await
        .unwrap();
    assert_eq!(jobs::claim(db, "w1", now).await.unwrap(), None);
    let again = jobs::claim(db, "w1", now + Duration::seconds(31))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((again.id, again.attempts), (keyed.id, 2));

    // Out of attempts: failed for good, the error kept, the key released.
    jobs::fail(db, again.id, "boom again", None, now)
        .await
        .unwrap();
    let state = jobs::state(db, again.id).await.unwrap().unwrap();
    assert_eq!(state.status, "failed");
    assert_eq!(state.last_error.as_deref(), Some("boom again"));
    assert!(
        jobs::enqueue(db, &job("a", Some("only-one")))
            .await
            .unwrap()
            .is_some()
    );

    // A worker that died: its job comes back after the stale cutoff.
    let orphan = jobs::claim(db, "dead", now).await.unwrap().unwrap();
    assert_eq!(
        jobs::reset_stale(db, now - Duration::minutes(15))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        jobs::reset_stale(db, now + Duration::seconds(1))
            .await
            .unwrap(),
        1
    );
    let reclaimed = jobs::claim(db, "alive", now).await.unwrap().unwrap();
    assert_eq!(reclaimed.id, orphan.id);
    jobs::complete(db, reclaimed.id, now).await.unwrap();
    assert_eq!(
        jobs::state(db, reclaimed.id).await.unwrap().unwrap().status,
        "done"
    );
    assert_eq!(
        jobs::prune_done(db, now + Duration::seconds(1))
            .await
            .unwrap(),
        2,
        "both finished jobs go; the failed one is kept"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn expired_files_are_deleted_and_kept_for_audit() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let runner = Runner::new(app.clone());
    let expiring = upload(&app).await;
    let lasting = upload_lasting(&app).await;

    // Not due yet: the run finds nothing to do.
    enqueue_recurring(&app, kinds::EXPIRE_FILES, Utc::now())
        .await
        .unwrap();
    assert!(runner.run_once(Utc::now()).await.unwrap());
    let still = haloid_db::files::find_by_id(&scratch.pool, expiring.id)
        .await
        .unwrap()
        .unwrap();
    assert!(still.deletion.is_none());

    // An hour on, it is.
    let later = Utc::now() + Duration::hours(2);
    enqueue_recurring(&app, kinds::EXPIRE_FILES, later)
        .await
        .unwrap();
    assert!(runner.run_once(later).await.unwrap());
    let expired = haloid_db::files::find_by_id(&scratch.pool, expiring.id)
        .await
        .unwrap()
        .expect("retained, not destroyed");
    assert_eq!(
        expired.deletion.map(|d| d.reason),
        Some(DeleteReason::Expired)
    );
    assert!(
        app.uploads
            .view(Actor::Guest, expired.id_encoded.as_str(), Utc::now())
            .await
            .is_err()
    );
    let untouched = haloid_db::files::find_by_id(&scratch.pool, lasting.id)
        .await
        .unwrap()
        .unwrap();
    assert!(untouched.deletion.is_none(), "a file with no expiry stays");

    // The bytes went to quarantine: every object of the expired file is
    // under `<data dir>/quarantine`, none in the served tree.
    let objects: Vec<String> = walk(dir.path())
        .into_iter()
        .filter(|p| p.contains(&expired.name))
        .collect();
    assert!(!objects.is_empty());
    let quarantine = dir.path().join("quarantine").display().to_string();
    assert!(
        objects.iter().all(|p| p.starts_with(&quarantine)),
        "{objects:?}"
    );

    scratch.drop_db().await;
}

async fn upload_lasting(app: &App) -> haloid_core::files::MediaFile {
    let bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/still.jpg"))
            .unwrap();
    app.uploads
        .upload(UploadCommand {
            actor: Actor::Guest,
            source: UploadSource::Stream {
                reader: Box::new(std::io::Cursor::new(bytes)),
                filename: Some("still.jpg".into()),
            },
            title: None,
            description: None,
            tags: Vec::new(),
            album: None,
            category: None,
            nsfw: None,
            expiration: None,
            width: None,
            uploader_ip: None,
        })
        .await
        .unwrap()
        .file
}

fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path.display().to_string());
        }
    }
    out
}

#[tokio::test]
async fn a_queued_purge_destroys_the_file() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let runner = Runner::new(app.clone());
    let file = upload(&app).await;

    enqueue_purge(&app, file.id.0, Utc::now()).await.unwrap();
    assert!(runner.run_once(Utc::now()).await.unwrap());
    assert!(
        haloid_db::files::find_by_id(&scratch.pool, file.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !walk(dir.path()).iter().any(|p| p.contains(&file.name)),
        "no bytes left anywhere"
    );
    // Purging what is already gone is done, not an error.
    enqueue_purge(&app, file.id.0, Utc::now()).await.unwrap();
    assert!(runner.run_once(Utc::now()).await.unwrap());
    assert!(!runner.run_once(Utc::now()).await.unwrap(), "queue empty");

    scratch.drop_db().await;
}

#[tokio::test]
async fn an_unknown_job_fails_and_is_retried_then_given_up() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let runner = Runner::new(app.clone());
    let id = jobs::enqueue(&scratch.pool, &job("no_such_kind", None))
        .await
        .unwrap()
        .unwrap();

    assert!(runner.run_once(Utc::now()).await.unwrap());
    let state = jobs::state(&scratch.pool, id).await.unwrap().unwrap();
    assert_eq!((state.status.as_str(), state.attempts), ("pending", 1));
    assert!(!runner.run_once(Utc::now()).await.unwrap(), "backing off");

    let later = Utc::now() + Duration::hours(2);
    assert!(runner.run_once(later).await.unwrap());
    let state = jobs::state(&scratch.pool, id).await.unwrap().unwrap();
    assert_eq!(state.status, "failed");
    assert!(state.last_error.unwrap().contains("no_such_kind"));

    scratch.drop_db().await;
}
