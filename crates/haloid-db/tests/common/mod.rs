//! The shared integration-test harness.
//!
//! Set `TEST_DATABASE_URL` to a server (no database component) and each test
//! creates and drops its own database, so tests stay isolated and order
//! independent. Without it these tests skip rather than fail: a contributor
//! without a database still gets a green `cargo test` for everything pure.
//!
//! Each test binary uses a different subset of this module.
#![allow(dead_code)]

use sqlx::mysql::MySqlPoolOptions;
use sqlx::{Executor, MySql, Pool};

/// A scratch database that drops itself.
pub struct Scratch {
    server: Pool<MySql>,
    pub pool: Pool<MySql>,
    name: String,
}

impl Scratch {
    pub async fn new() -> Option<Self> {
        let base = std::env::var("TEST_DATABASE_URL").ok()?;
        let base = base.trim_end_matches('/').to_string();
        // The name carries its creation time so that the sweep below can tell
        // an abandoned database from one a concurrent test is still using.
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is before 1970")
            .as_secs();
        let name = format!("haloid_test_{created}_{}", uuid::Uuid::new_v4().simple());

        let server = MySqlPoolOptions::new()
            .max_connections(1)
            .connect(&base)
            .await
            .expect("connect to TEST_DATABASE_URL");
        sweep_abandoned(&server, created).await;
        server
            .execute(format!("CREATE DATABASE `{name}`").as_str())
            .await
            .expect("create scratch database");

        let pool = MySqlPoolOptions::new()
            .max_connections(8)
            .connect(&format!("{base}/{name}"))
            .await
            .expect("connect to scratch database");

        Some(Self { server, pool, name })
    }

    /// A scratch database with every migration applied.
    pub async fn migrated() -> Option<Self> {
        let scratch = Self::new().await?;
        haloid_db::migrate(&scratch.pool)
            .await
            .expect("migrations should apply");
        Some(scratch)
    }

    pub async fn drop_db(&self) {
        let _ = self
            .server
            .execute(format!("DROP DATABASE IF EXISTS `{}`", self.name).as_str())
            .await;
    }
}

/// Drop scratch databases left behind by earlier runs.
///
/// A test that panics never reaches its own cleanup — Rust has no async drop —
/// so without this the server slowly fills with dead databases. Anything older
/// than an hour cannot belong to a running test.
async fn sweep_abandoned(server: &Pool<MySql>, now: u64) {
    const MAX_AGE_SECS: u64 = 3600;

    let Ok(names) = sqlx::query_as::<_, (String,)>(
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA
         WHERE SCHEMA_NAME LIKE 'haloid\\_test\\_%'",
    )
    .fetch_all(server)
    .await
    else {
        return;
    };

    for (name,) in names {
        let created = name
            .strip_prefix("haloid_test_")
            .and_then(|rest| rest.split('_').next())
            .and_then(|secs| secs.parse::<u64>().ok());
        // An unparseable name predates this scheme; treat it as abandoned.
        let stale = created.is_none_or(|created| now.saturating_sub(created) > MAX_AGE_SECS);
        if stale {
            let _ = server
                .execute(format!("DROP DATABASE IF EXISTS `{name}`").as_str())
                .await;
        }
    }
}

/// Obtain a scratch database, or skip the test.
///
/// Skipping reports as *passing*, which makes a missing `TEST_DATABASE_URL`
/// invisible — the whole integration suite can silently not run. That is an
/// acceptable local convenience and an unacceptable CI result, so under `CI`
/// the absence of the variable is a failure rather than a skip.
#[macro_export]
macro_rules! scratch_or_skip {
    () => {
        match $crate::common::Scratch::new().await {
            Some(scratch) => scratch,
            None => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "TEST_DATABASE_URL is not set, so the integration tests \
                     would silently skip. In CI that is a failure: a green run \
                     must mean these actually executed."
                );
                eprintln!("skipping: TEST_DATABASE_URL is not set");
                return;
            }
        }
    };
}

/// [`scratch_or_skip!`], with the migrations already applied.
#[macro_export]
macro_rules! migrated_or_skip {
    () => {
        match $crate::common::Scratch::migrated().await {
            Some(scratch) => scratch,
            None => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "TEST_DATABASE_URL is not set, so the integration tests \
                     would silently skip. In CI that is a failure: a green run \
                     must mean these actually executed."
                );
                eprintln!("skipping: TEST_DATABASE_URL is not set");
                return;
            }
        }
    };
}
