//! The live settings: one shared, hot-reloadable snapshot.
//!
//! Readers call [`SettingsHandle::get`], a pointer load with no lock and no
//! database round-trip, and get an `Arc<Settings>` that stays internally
//! consistent however long they hold it. A reload builds a fresh snapshot
//! and swaps it in.
//!
//! Three layers, highest wins: the compiled defaults, the `ih_settings`
//! table, the environment. Loading never fails on a bad value — it falls back
//! and warns — but an administrator's write is checked strictly first, so a
//! write either takes effect completely or not at all.

use arc_swap::ArcSwap;
use haloid_core::settings::{SettingWarning, Settings, env};
use haloid_core::{DomainError, FieldError};
use haloid_db::Db;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Cheap to clone; every clone shares the one snapshot.
#[derive(Clone)]
pub struct SettingsHandle {
    inner: Arc<Inner>,
}

struct Inner {
    current: ArcSwap<Settings>,
    db: Db,
    /// Settings pinned by the environment, read once at startup: the process
    /// environment does not change under a running server.
    env: BTreeMap<String, String>,
    /// The warnings last logged, so a periodic reload that finds the same bad
    /// row does not log it every 30 seconds forever.
    last_warnings: Mutex<Vec<SettingWarning>>,
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

impl SettingsHandle {
    /// Load the settings, with `env` as the pinned overrides — usually
    /// [`env_from_process`].
    pub async fn load(db: Db, env: BTreeMap<String, String>) -> Result<Self, DomainError> {
        let handle = Self {
            inner: Arc::new(Inner {
                current: ArcSwap::from_pointee(Settings::default()),
                db,
                env,
                last_warnings: Mutex::new(Vec::new()),
            }),
        };
        handle.reload().await?;
        Ok(handle)
    }

    /// The current snapshot.
    pub fn get(&self) -> Arc<Settings> {
        self.inner.current.load_full()
    }

    /// The setting keys the environment pins, which the admin UI should show
    /// as read-only.
    pub fn pinned_keys(&self) -> impl Iterator<Item = &str> {
        self.inner.env.keys().map(String::as_str)
    }

    /// Rebuild from the database and swap the new snapshot in.
    pub async fn reload(&self) -> Result<(), DomainError> {
        let stored = haloid_db::settings::load_raw(&self.inner.db)
            .await
            .map_err(database)?;
        let load = Settings::from_map(&env::layer(&stored, &self.inner.env));
        self.log_if_new(load.warnings);
        self.inner.current.store(Arc::new(load.settings));
        Ok(())
    }

    fn log_if_new(&self, warnings: Vec<SettingWarning>) {
        let mut last = self
            .inner
            .last_warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *last == warnings {
            return;
        }
        for w in &warnings {
            tracing::warn!(setting = %w.key, reason = %w.reason, "setting ignored, using default");
        }
        *last = warnings;
    }

    /// An administrator's write: checked, stored, and live before this
    /// returns. All or nothing — one bad value refuses the whole write.
    pub async fn update(
        &self,
        changes: &BTreeMap<String, String>,
    ) -> Result<Arc<Settings>, DomainError> {
        let stored = haloid_db::settings::load_raw(&self.inner.db)
            .await
            .map_err(database)?;
        Settings::check_write(&stored, &self.inner.env, changes).map_err(|problems| {
            DomainError::InvalidParams(
                problems
                    .into_iter()
                    .map(|p| FieldError {
                        field: p.key,
                        reason: p.reason,
                    })
                    .collect(),
            )
        })?;
        haloid_db::settings::save(&self.inner.db, changes)
            .await
            .map_err(database)?;
        self.reload().await?;
        Ok(self.get())
    }

    /// Return settings to their compiled defaults (or their environment
    /// value, where pinned).
    pub async fn reset(&self, keys: &[String]) -> Result<Arc<Settings>, DomainError> {
        haloid_db::settings::reset(&self.inner.db, keys)
            .await
            .map_err(database)?;
        self.reload().await?;
        Ok(self.get())
    }

    /// Reload every `period`, so an edit made straight in SQL converges
    /// without a restart. A failed reload keeps the last good snapshot and
    /// tries again next tick.
    pub fn spawn_reload_ticker(&self, period: Duration) -> tokio::task::JoinHandle<()> {
        let handle = self.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(period);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticks.tick().await; // The first tick is immediate; we just loaded.
            loop {
                ticks.tick().await;
                if let Err(error) = handle.reload().await {
                    tracing::warn!(%error, "settings reload failed; keeping the current settings");
                }
            }
        })
    }
}

/// The settings pinned in this process's environment.
pub fn env_from_process() -> BTreeMap<String, String> {
    env::overrides(|var| std::env::var(var).ok())
}

/// How often the server converges manual SQL edits.
pub const RELOAD_PERIOD: Duration = Duration::from_secs(30);
