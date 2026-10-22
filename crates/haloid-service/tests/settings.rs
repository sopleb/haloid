//! The settings runtime, against a real MariaDB.

// The database crate's harness, shared rather than copied: one definition of
// how scratch databases are made, swept and skipped.
#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use haloid_core::DomainError;
use haloid_core::settings::Settings;
use haloid_service::settings::SettingsHandle;
use std::collections::BTreeMap;
use std::time::Duration;

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

async fn set_row(db: &haloid_db::Db, name: &str, value: &str) {
    sqlx::query(
        "INSERT INTO ih_settings (name, value) VALUES (?, ?)
         ON DUPLICATE KEY UPDATE value = VALUES(value)",
    )
    .bind(name)
    .bind(value)
    .execute(db)
    .await
    .expect("write a settings row");
}

#[tokio::test]
async fn a_fresh_install_runs_on_the_compiled_defaults() {
    let scratch = migrated_or_skip!();
    let handle = SettingsHandle::load(scratch.pool.clone(), BTreeMap::new())
        .await
        .expect("load");
    assert_eq!(*handle.get(), Settings::default());
    scratch.drop_db().await;
}

#[tokio::test]
async fn a_manual_sql_edit_is_picked_up_without_a_restart() {
    // The plan's gate for this chunk: write a row, observe the change.
    let scratch = migrated_or_skip!();
    let handle = SettingsHandle::load(scratch.pool.clone(), BTreeMap::new())
        .await
        .unwrap();
    let before = handle.get();

    set_row(&scratch.pool, "site.name", "Edited by hand").await;
    handle.reload().await.expect("reload");

    assert_eq!(handle.get().site.name, "Edited by hand");
    assert_eq!(
        before.site.name,
        Settings::default().site.name,
        "a reader holding the old snapshot keeps a consistent one"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn the_ticker_converges_without_anyone_calling_reload() {
    let scratch = migrated_or_skip!();
    let handle = SettingsHandle::load(scratch.pool.clone(), BTreeMap::new())
        .await
        .unwrap();
    let ticker = handle.spawn_reload_ticker(Duration::from_millis(50));

    set_row(&scratch.pool, "uploads.enabled", "false").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while handle.get().uploads.enabled {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the ticker never picked up the change"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    ticker.abort();
    scratch.drop_db().await;
}

#[tokio::test]
async fn an_admin_write_is_stored_and_live_immediately() {
    let scratch = migrated_or_skip!();
    let handle = SettingsHandle::load(scratch.pool.clone(), BTreeMap::new())
        .await
        .unwrap();

    let updated = handle
        .update(&map(&[
            ("site.name", "Pictures"),
            ("moderation.mode", "all"),
        ]))
        .await
        .expect("a valid write");
    assert_eq!(updated.site.name, "Pictures");
    assert_eq!(handle.get().site.name, "Pictures");

    // And it is durable: a second process loading cold sees it.
    let cold = SettingsHandle::load(scratch.pool.clone(), BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(cold.get().site.name, "Pictures");

    scratch.drop_db().await;
}

#[tokio::test]
async fn an_invalid_write_changes_nothing() {
    let scratch = migrated_or_skip!();
    let handle = SettingsHandle::load(scratch.pool.clone(), BTreeMap::new())
        .await
        .unwrap();

    let error = handle
        .update(&map(&[
            ("site.name", "Would be fine"),
            ("uploads.max_size_user", "ten megabytes"),
        ]))
        .await
        .expect_err("a bad value must refuse the write");
    match error {
        DomainError::InvalidParams(fields) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].field, "uploads.max_size_user");
        }
        other => panic!("expected InvalidParams, got {other:?}"),
    }

    // All or nothing: the valid half was not stored either.
    handle.reload().await.unwrap();
    assert_eq!(handle.get().site.name, Settings::default().site.name);
    assert!(
        haloid_db::settings::load_raw(&scratch.pool)
            .await
            .unwrap()
            .is_empty()
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn the_environment_pins_a_setting_above_the_database() {
    let scratch = migrated_or_skip!();
    set_row(&scratch.pool, "site.name", "From db").await;
    let env = map(&[("site.name", "From env")]);
    let handle = SettingsHandle::load(scratch.pool.clone(), env)
        .await
        .unwrap();

    assert_eq!(handle.get().site.name, "From env");
    assert_eq!(handle.pinned_keys().collect::<Vec<_>>(), vec!["site.name"]);

    let error = handle
        .update(&map(&[("site.name", "From admin")]))
        .await
        .expect_err("a pinned key cannot be written");
    assert!(matches!(error, DomainError::InvalidParams(_)));

    scratch.drop_db().await;
}

#[tokio::test]
async fn resetting_restores_the_default() {
    let scratch = migrated_or_skip!();
    let handle = SettingsHandle::load(scratch.pool.clone(), BTreeMap::new())
        .await
        .unwrap();
    handle
        .update(&map(&[("site.name", "Pictures")]))
        .await
        .unwrap();

    handle.reset(&["site.name".to_string()]).await.unwrap();
    assert_eq!(handle.get().site.name, Settings::default().site.name);

    scratch.drop_db().await;
}
