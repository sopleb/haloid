//! Connection pool and migration entry points.

use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::{ConnectOptions, MySql, Pool};
use std::str::FromStr;
use std::time::Duration;

pub type Db = Pool<MySql>;

/// Open a pool against `url`.
///
/// `max_connections` wants to be a little above the job runner's worker count
/// plus the expected request concurrency; the upload path holds a connection
/// only around its transaction, never while bytes are moving.
pub async fn connect(url: &str, max_connections: u32) -> Result<Db, sqlx::Error> {
    let options = MySqlConnectOptions::from_str(url)?
        // Statements slower than this are logged at warn. The three slow
        // things in an upload are vips, ffmpeg and S3 — none of them are SQL,
        // so a slow statement here is always worth seeing.
        .log_slow_statements(log::LevelFilter::Warn, Duration::from_millis(500));

    MySqlPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
}

/// A round trip to the database, for readiness checks.
pub async fn ping(db: &Db) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(db).await.map(drop)
}

/// Apply any outstanding migrations. Idempotent.
pub async fn migrate(db: &Db) -> Result<(), sqlx::migrate::MigrateError> {
    crate::MIGRATOR.run(db).await
}
