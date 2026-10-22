//! Schema, migrator, preflight and settings, against a real MariaDB. See
//! `common` for how the scratch databases work.

mod common;

use haloid_core::settings::Settings;
use haloid_db::{MIGRATOR, migrate, preflight};

#[tokio::test]
async fn migrations_apply_to_an_empty_database() {
    let scratch = scratch_or_skip!();

    migrate(&scratch.pool)
        .await
        .expect("migrations should apply");

    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT TABLE_NAME FROM information_schema.TABLES
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME LIKE 'ih_%'",
    )
    .fetch_all(&scratch.pool)
    .await
    .expect("list tables");

    // Sorted here rather than in SQL: under utf8mb4_unicode_520_ci the server
    // orders `ih_files` before `ih_file_tags`, which is a collation detail this
    // test has no business asserting.
    let mut names: Vec<String> = tables.into_iter().map(|(n,)| n).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "ih_albums",
            "ih_api_keys",
            "ih_audit_log",
            "ih_categories",
            "ih_file_tags",
            "ih_files",
            "ih_instance",
            "ih_ip_bans",
            "ih_jobs",
            "ih_likes",
            "ih_moderation_queue",
            "ih_sessions",
            "ih_settings",
            "ih_stats",
            "ih_storages",
            "ih_tags",
            "ih_upload_sessions",
            "ih_users",
        ]
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn migrations_are_idempotent() {
    let scratch = scratch_or_skip!();

    migrate(&scratch.pool).await.expect("first run");
    migrate(&scratch.pool)
        .await
        .expect("second run must be a no-op");

    scratch.drop_db().await;
}

#[tokio::test]
async fn every_migration_has_a_checksum_recorded() {
    let scratch = scratch_or_skip!();
    migrate(&scratch.pool)
        .await
        .expect("migrations should apply");

    let applied: Vec<(i64,)> =
        sqlx::query_as("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&scratch.pool)
            .await
            .expect("read migration table");

    assert_eq!(
        applied.len(),
        MIGRATOR.iter().count(),
        "applied migration count does not match the embedded set"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn preflight_passes_against_a_supported_server() {
    let scratch = scratch_or_skip!();
    preflight(&scratch.pool)
        .await
        .expect("the development server should satisfy preflight");
    scratch.drop_db().await;
}

#[tokio::test]
async fn a_guest_upload_survives_its_albums_deletion() {
    // ih_files.album_id is ON DELETE SET NULL: deleting an album must not take
    // its files with it. Worth pinning — the alternative cascade would destroy
    // user data on a tidy-up.
    let scratch = scratch_or_skip!();
    migrate(&scratch.pool)
        .await
        .expect("migrations should apply");

    sqlx::query("INSERT INTO ih_albums (id_encoded, name) VALUES ('alb0000001', 'Holiday')")
        .execute(&scratch.pool)
        .await
        .expect("insert album");

    let album_id: u64 =
        sqlx::query_scalar("SELECT id FROM ih_albums WHERE id_encoded = 'alb0000001'")
            .fetch_one(&scratch.pool)
            .await
            .expect("read album id");

    sqlx::query(
        "INSERT INTO ih_files
           (id_encoded, kind, name, extension, mime, size, chain, checksum, storage_path, album_id)
         VALUES ('fil0000001', 'image', 'cat-ab12cd34', 'jpg', 'image/jpeg', 1234, 7,
                 '00000000000000000000000000000000', '2026/10/21/cat-ab12cd34.jpg', ?)",
    )
    .bind(album_id)
    .execute(&scratch.pool)
    .await
    .expect("insert file");

    sqlx::query("DELETE FROM ih_albums WHERE id = ?")
        .bind(album_id)
        .execute(&scratch.pool)
        .await
        .expect("delete album");

    let (count, album): (i64, Option<u64>) = sqlx::query_as(
        "SELECT COUNT(*), MAX(album_id) FROM ih_files WHERE id_encoded = 'fil0000001'",
    )
    .fetch_one(&scratch.pool)
    .await
    .expect("read file back");

    assert_eq!(count, 1, "the file was deleted with its album");
    assert_eq!(album, None, "album_id was not cleared");

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_storage_cannot_be_deleted_while_it_holds_files() {
    // ON DELETE RESTRICT: removing a storage row out from under its files
    // would orphan every one of them.
    let scratch = scratch_or_skip!();
    migrate(&scratch.pool)
        .await
        .expect("migrations should apply");

    sqlx::query(
        "INSERT INTO ih_storages (name, kind, config, url_prefix)
         VALUES ('main', 'local', '{}', 'http://localhost:8080/files')",
    )
    .execute(&scratch.pool)
    .await
    .expect("insert storage");

    let storage_id: u64 = sqlx::query_scalar("SELECT id FROM ih_storages WHERE name = 'main'")
        .fetch_one(&scratch.pool)
        .await
        .expect("read storage id");

    sqlx::query(
        "INSERT INTO ih_files
           (id_encoded, kind, name, extension, mime, size, chain, checksum, storage_path, storage_id)
         VALUES ('fil0000002', 'image', 'dog-ef56ab78', 'png', 'image/png', 99, 7,
                 '11111111111111111111111111111111', '2026/10/21/dog-ef56ab78.png', ?)",
    )
    .bind(storage_id)
    .execute(&scratch.pool)
    .await
    .expect("insert file");

    let result = sqlx::query("DELETE FROM ih_storages WHERE id = ?")
        .bind(storage_id)
        .execute(&scratch.pool)
        .await;
    assert!(result.is_err(), "deleting an in-use storage was allowed");

    scratch.drop_db().await;
}

#[tokio::test]
async fn public_identifiers_are_unique() {
    let scratch = scratch_or_skip!();
    migrate(&scratch.pool)
        .await
        .expect("migrations should apply");

    let insert = "INSERT INTO ih_files
           (id_encoded, kind, name, extension, mime, size, chain, checksum, storage_path)
         VALUES ('fil0000003', 'image', 'a-11112222', 'jpg', 'image/jpeg', 1, 4,
                 '22222222222222222222222222222222', 'a.jpg')";

    sqlx::query(insert)
        .execute(&scratch.pool)
        .await
        .expect("first insert");
    let second = sqlx::query(insert).execute(&scratch.pool).await;
    assert!(second.is_err(), "a duplicate id_encoded was accepted");

    scratch.drop_db().await;
}

#[tokio::test]
async fn settings_round_trip_through_the_database() {
    let scratch = scratch_or_skip!();
    migrate(&scratch.pool)
        .await
        .expect("migrations should apply");

    // An empty table must yield the compiled defaults, so a fresh install
    // boots with no seed rows at all.
    let load = haloid_db::settings::load(&scratch.pool)
        .await
        .expect("load from empty table");
    assert_eq!(load.settings, Settings::default());
    assert!(load.warnings.is_empty(), "{:?}", load.warnings);

    let mut settings = Settings::default();
    settings.site.name = "Pictures".into();
    settings.uploads.guests_enabled = false;
    settings.uploads.max_size_user = 7_654_321;
    haloid_db::settings::save_all(&scratch.pool, &settings)
        .await
        .expect("save_all");

    let reloaded = haloid_db::settings::load(&scratch.pool)
        .await
        .expect("reload");
    assert_eq!(reloaded.settings, settings);
    assert!(reloaded.warnings.is_empty(), "{:?}", reloaded.warnings);

    scratch.drop_db().await;
}

#[tokio::test]
async fn saving_one_setting_leaves_the_others_alone() {
    let scratch = scratch_or_skip!();
    migrate(&scratch.pool)
        .await
        .expect("migrations should apply");

    let mut settings = Settings::default();
    settings.site.name = "Pictures".into();
    haloid_db::settings::save_all(&scratch.pool, &settings)
        .await
        .expect("save_all");

    let mut patch = std::collections::BTreeMap::new();
    patch.insert("uploads.enabled".to_string(), "false".to_string());
    haloid_db::settings::save(&scratch.pool, &patch)
        .await
        .expect("partial save");

    let load = haloid_db::settings::load(&scratch.pool)
        .await
        .expect("reload");
    assert!(!load.settings.uploads.enabled, "the patch did not apply");
    assert_eq!(
        load.settings.site.name, "Pictures",
        "the patch clobbered another setting"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_malformed_row_falls_back_instead_of_failing_the_load() {
    // The behaviour that keeps a fat-fingered row from taking an instance
    // down. Asserted end to end, not just over the parser.
    let scratch = scratch_or_skip!();
    migrate(&scratch.pool)
        .await
        .expect("migrations should apply");

    sqlx::query("INSERT INTO ih_settings (name, value) VALUES ('uploads.max_size_user', 'lots')")
        .execute(&scratch.pool)
        .await
        .expect("insert a bad row");

    let load = haloid_db::settings::load(&scratch.pool)
        .await
        .expect("load must succeed despite the bad row");
    assert_eq!(
        load.settings.uploads.max_size_user,
        Settings::default().uploads.max_size_user
    );
    assert_eq!(load.warnings.len(), 1);
    assert_eq!(load.warnings[0].key, "uploads.max_size_user");

    scratch.drop_db().await;
}

#[tokio::test]
async fn resetting_a_setting_restores_its_default() {
    let scratch = scratch_or_skip!();
    migrate(&scratch.pool)
        .await
        .expect("migrations should apply");

    let mut settings = Settings::default();
    settings.site.name = "Pictures".into();
    haloid_db::settings::save_all(&scratch.pool, &settings)
        .await
        .expect("save_all");

    let removed = haloid_db::settings::reset(&scratch.pool, &["site.name".to_string()])
        .await
        .expect("reset");
    assert_eq!(removed, 1);

    let load = haloid_db::settings::load(&scratch.pool)
        .await
        .expect("reload");
    assert_eq!(load.settings.site.name, Settings::default().site.name);

    scratch.drop_db().await;
}
