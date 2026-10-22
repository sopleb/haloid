//! The storages repository, against a real MariaDB.

mod common;

use chrono::{SubsecRound, Utc};
use haloid_core::files::{DeleteReason, Deletion};
use haloid_core::ids::StorageId;
use haloid_core::media::{Chain, MediaKind};
use haloid_core::storage::{StorageKind, StoragePathMode, StorageRecord};
use haloid_db::files::{self, NewFile};
use haloid_db::storages::{self, StorageDetails};
use haloid_db::{Db, DbError};

fn details(name: &str, priority: i32, capacity: Option<u64>) -> StorageDetails {
    StorageDetails {
        name: name.into(),
        kind: StorageKind::Local,
        config: r#"{"base_path":"/srv/haloid"}"#.into(),
        url_prefix: format!("https://img.example.com/{name}"),
        path_mode: StoragePathMode::DateFolder,
        path_template: None,
        capacity_bytes: capacity,
        is_active: true,
        priority,
    }
}

async fn reload(db: &Db, id: StorageId) -> StorageRecord {
    storages::find_by_id(db, id).await.unwrap().unwrap()
}

/// A ready 1000-byte file on `storage`.
async fn stored_file(db: &Db, storage: StorageId) -> haloid_core::ids::FileId {
    let new = NewFile {
        kind: MediaKind::Image,
        name: "cat-ab12cd34",
        extension: "jpg",
        original_extension: None,
        mime: "image/jpeg",
        original_filename: None,
        title: None,
        description: None,
        size: 1000,
        width: 10,
        height: 10,
        duration_ms: None,
        is_animated: false,
        frame_count: None,
        chain: Chain::from_bits(5),
        thumb_size: None,
        medium_size: None,
        frame_size: None,
        checksum: "0123456789abcdef0123456789abcdef",
        source_checksum: None,
        sha256: None,
        storage_id: Some(storage),
        storage_mode: StoragePathMode::DateFolder,
        user_id: None,
        album_id: None,
        category_id: None,
        nsfw: false,
        is_approved: true,
        is_360: false,
        exif: None,
        uploader_ip: None,
        delete_token: None,
        expires_at: None,
    };
    let file = files::create_pending(db, &new, |id| format!("{id}.jpg"))
        .await
        .unwrap();
    files::mark_ready(db, file.id).await.unwrap();
    file.id
}

#[tokio::test]
async fn a_storage_reads_back_and_names_are_unique() {
    let scratch = migrated_or_skip!();

    let created = storages::create(&scratch.pool, &details("main", 0, Some(5_000)))
        .await
        .unwrap();
    assert_eq!(created.name, "main");
    assert_eq!(created.kind, StorageKind::Local);
    assert!(created.config.contains("/srv/haloid"), "{}", created.config);
    assert_eq!(created.capacity_bytes, Some(5_000));
    assert_eq!((created.used_bytes, created.file_count), (0, 0));
    assert!(created.is_active);
    assert_eq!(created.retired_at, None);

    assert!(matches!(
        storages::create(&scratch.pool, &details("MAIN", 0, None)).await,
        Err(DbError::Conflict("name"))
    ));

    let mut changed = details("primary", 5, None);
    changed.url_prefix = "https://cdn.example.com".into();
    assert!(
        storages::update(&scratch.pool, created.id, &changed)
            .await
            .unwrap()
    );
    let reloaded = reload(&scratch.pool, created.id).await;
    assert_eq!(reloaded.name, "primary");
    assert_eq!(reloaded.priority, 5);
    assert_eq!(reloaded.capacity_bytes, None);

    scratch.drop_db().await;
}

#[tokio::test]
async fn usage_counts_bytes_held_until_purge() {
    // Soft-deleted files still occupy the storage; only a purge frees it.
    let scratch = migrated_or_skip!();
    let main = storages::create(&scratch.pool, &details("main", 0, None))
        .await
        .unwrap();
    let file = stored_file(&scratch.pool, main.id).await;
    stored_file(&scratch.pool, main.id).await;

    let s = reload(&scratch.pool, main.id).await;
    assert_eq!((s.used_bytes, s.file_count), (2000, 2));

    let deletion = Deletion {
        at: Utc::now().trunc_subsecs(0),
        by: None,
        reason: DeleteReason::Expired,
    };
    files::soft_delete(&scratch.pool, file, &deletion)
        .await
        .unwrap();
    let s = reload(&scratch.pool, main.id).await;
    assert_eq!(
        (s.used_bytes, s.file_count),
        (2000, 2),
        "a soft delete freed space"
    );

    files::purge(&scratch.pool, file).await.unwrap();
    let s = reload(&scratch.pool, main.id).await;
    assert_eq!((s.used_bytes, s.file_count), (1000, 1));

    scratch.drop_db().await;
}

#[tokio::test]
async fn uploads_go_to_the_preferred_storage_with_room() {
    let scratch = migrated_or_skip!();
    let small = storages::create(&scratch.pool, &details("small", 10, Some(1_500)))
        .await
        .unwrap();
    let roomy = storages::create(&scratch.pool, &details("roomy", 1, Some(1_000_000)))
        .await
        .unwrap();
    let unlimited = storages::create(&scratch.pool, &details("unlimited", 1, None))
        .await
        .unwrap();
    storages::create(&scratch.pool, &details("low", 0, None))
        .await
        .unwrap();

    let pick = |size| storages::pick_for_upload(&scratch.pool, size);
    assert_eq!(
        pick(1000).await.unwrap().map(|s| s.id),
        Some(small.id),
        "highest priority with room wins"
    );

    stored_file(&scratch.pool, small.id).await;
    assert_eq!(
        pick(1000).await.unwrap().map(|s| s.id),
        Some(unlimited.id),
        "full at its priority, so the next tier; unlimited beats finite"
    );

    storages::set_active(&scratch.pool, unlimited.id, false)
        .await
        .unwrap();
    assert_eq!(pick(1000).await.unwrap().map(|s| s.id), Some(roomy.id));

    storages::retire(&scratch.pool, roomy.id, Utc::now().trunc_subsecs(0))
        .await
        .unwrap();
    let retired = reload(&scratch.pool, roomy.id).await;
    assert!(retired.retired_at.is_some());
    assert!(!retired.is_active, "a retired storage takes no uploads");
    assert_eq!(
        pick(1000).await.unwrap().map(|s| s.name),
        Some("low".to_string())
    );

    let listed: Vec<String> = storages::list(&scratch.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        listed,
        vec!["small", "roomy", "unlimited", "low"],
        "retired storages stay listed: they still serve their files"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn nothing_is_picked_when_no_storage_has_room() {
    let scratch = migrated_or_skip!();
    assert!(
        storages::pick_for_upload(&scratch.pool, 1)
            .await
            .unwrap()
            .is_none()
    );
    storages::create(&scratch.pool, &details("tiny", 0, Some(10)))
        .await
        .unwrap();
    assert!(
        storages::pick_for_upload(&scratch.pool, 11)
            .await
            .unwrap()
            .is_none()
    );
    scratch.drop_db().await;
}
