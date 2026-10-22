//! Reading and writing the settings rows.
//!
//! This layer moves strings. Interpreting them is
//! `haloid_core::settings::map`, which falls back and warns rather than
//! failing, so a malformed row cannot stop the server booting.

use crate::Db;
use haloid_core::settings::{Settings, SettingsLoad};
use std::collections::BTreeMap;

/// Every stored setting, as written.
pub async fn load_raw(db: &Db) -> Result<BTreeMap<String, String>, sqlx::Error> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as("SELECT name, value FROM ih_settings")
        .fetch_all(db)
        .await?;

    Ok(rows
        .into_iter()
        .map(|(name, value)| (name, value.unwrap_or_default()))
        .collect())
}

/// Load and interpret. Warnings are the caller's to log.
pub async fn load(db: &Db) -> Result<SettingsLoad, sqlx::Error> {
    Ok(Settings::from_map(&load_raw(db).await?))
}

/// Write some settings, leaving the rest alone.
///
/// One statement per key in a transaction: the set is small and bounded by the
/// number of settings that exist, and a partial write would leave an instance
/// in a configuration nobody chose.
pub async fn save(db: &Db, values: &BTreeMap<String, String>) -> Result<(), sqlx::Error> {
    if values.is_empty() {
        return Ok(());
    }

    let mut tx = db.begin().await?;
    for (name, value) in values {
        sqlx::query(
            "INSERT INTO ih_settings (name, value) VALUES (?, ?)
             ON DUPLICATE KEY UPDATE value = VALUES(value)",
        )
        .bind(name)
        .bind(value)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Write every setting. Used by the installer and by a full settings save.
pub async fn save_all(db: &Db, settings: &Settings) -> Result<(), sqlx::Error> {
    save(db, &settings.to_map()).await
}

/// Remove settings so they fall back to their compiled defaults.
pub async fn reset(db: &Db, names: &[String]) -> Result<u64, sqlx::Error> {
    if names.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; names.len()].join(",");
    let sql = format!("DELETE FROM ih_settings WHERE name IN ({placeholders})");
    let mut query = sqlx::query(&sql);
    for name in names {
        query = query.bind(name);
    }
    Ok(query.execute(db).await?.rows_affected())
}
