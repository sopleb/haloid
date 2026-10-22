//! The upload pipeline and file lifecycle, end to end: a real MariaDB, real
//! local storage in a temporary directory, and the real media tools.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use chrono::Utc;
use haloid_core::DomainError;
use haloid_core::account::{User, UserStatus};
use haloid_core::auth::{Actor, Role};
use haloid_core::files::{AlbumPrivacy, DeleteReason};
use haloid_core::media::{Derivative, MediaKind};
use haloid_core::storage::{StorageKind, StoragePathMode};
use haloid_db::Db;
use haloid_media::{MediaConfig, MediaToolkit};
use haloid_service::files::{DeleteAs, Files};
use haloid_service::licensing::Licensing;
use haloid_service::settings::SettingsHandle;
use haloid_service::upload::{UploadCommand, UploadSource, Uploads, object_keys};
use haloid_storage::pool::StoragePool;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/media")
        .join(name)
}

struct World {
    db: Db,
    uploads: Uploads,
    files: Files,
    served: PathBuf,
    quarantine: PathBuf,
    _dir: TempDir,
}

async fn world(db: &Db) -> World {
    world_with(db, &[]).await
}

/// A signing key for test licences; the server trusts its public half.
const SIGNER: [u8; 32] = [42; 32];

/// A perpetual Pro licence for this database's instance.
async fn pro(db: &Db) -> Arc<Licensing> {
    haloid_db::instance::ensure(db, "inst_test").await.unwrap();
    let key = haloid_licence::sign(
        &haloid_licence::Licence {
            id: "lic_test".into(),
            instance: "inst_test".into(),
            licensee: "Tests".into(),
            kind: haloid_licence::Kind::Perpetual,
            issued_at: Utc::now(),
            expires_at: None,
        },
        &SIGNER,
    );
    Arc::new(
        Licensing::load(
            db.clone(),
            vec![haloid_licence::public_key(&SIGNER)],
            Some(key),
        )
        .await
        .unwrap(),
    )
}

async fn world_with(db: &Db, settings_rows: &[(&str, &str)]) -> World {
    let dir = TempDir::new().unwrap();
    let served = dir.path().join("served");
    let quarantine = dir.path().join("quarantine");
    let config = serde_json::json!({
        "base_path": served,
        "quarantine_path": quarantine,
    })
    .to_string();
    haloid_db::storages::create(
        db,
        &haloid_db::storages::StorageDetails {
            name: "main".into(),
            kind: StorageKind::Local,
            config,
            url_prefix: "https://img.example.com/files".into(),
            path_mode: StoragePathMode::DateFolder,
            path_template: None,
            capacity_bytes: None,
            is_active: true,
            priority: 0,
        },
    )
    .await
    .unwrap();
    let records = haloid_db::storages::list(db).await.unwrap();
    let pool = Arc::new(StoragePool::from_records(&records, dir.path(), &[]).unwrap());

    let settings = SettingsHandle::load(db.clone(), BTreeMap::new())
        .await
        .unwrap();
    if !settings_rows.is_empty() {
        settings
            .update(
                &settings_rows
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
            .await
            .unwrap();
    }
    let media = Arc::new(MediaToolkit::new(MediaConfig::default()));
    World {
        db: db.clone(),
        // Pro, so audio is accepted; see the Community test below.
        uploads: Uploads::new(
            db.clone(),
            settings.clone(),
            media,
            pool.clone(),
            dir.path().join("tmp"),
        )
        .with_licensing(pro(db).await),
        files: Files::new(db.clone(), pool),
        served,
        quarantine,
        _dir: dir,
    }
}

async fn user(db: &Db, name: &str, role: Role) -> User {
    haloid_db::users::create(
        db,
        &haloid_db::users::NewUser {
            username: name,
            email: None,
            password_hash: None,
            role,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap()
}

fn command(actor: Actor, name: &str) -> UploadCommand {
    let bytes = std::fs::read(fixture(name)).unwrap();
    UploadCommand {
        actor,
        source: UploadSource::Stream {
            reader: Box::new(std::io::Cursor::new(bytes)),
            filename: Some(name.to_string()),
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
    }
}

fn exists_under(root: &Path, key: &str) -> bool {
    root.join(key).exists()
}

/// No temporary file outlives its upload, however it ended.
fn assert_no_temps(world: &World) {
    let tmp = world._dir.path().join("tmp");
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .map(|d| d.map(|e| e.unwrap().file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "temporary files left behind: {left:?}");
}

#[tokio::test]
async fn a_guest_photo_is_stored_derived_counted_and_linked() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool).await;

    let uploaded = w
        .uploads
        .upload(command(Actor::Guest, "still.jpg"))
        .await
        .unwrap();
    let file = &uploaded.file;
    assert!(file.is_live(Utc::now()));
    assert_eq!(file.kind, MediaKind::Image);
    assert_eq!((file.width, file.height), (32, 16));
    assert_eq!(
        file.chain.bits(),
        5,
        "image and thumb; too small for a medium"
    );
    assert_eq!(file.user_id, None);
    assert_eq!(file.original_filename.as_deref(), Some("still.jpg"));
    assert!(file.name.starts_with("still-"), "{}", file.name);
    assert!(!uploaded.duplicate);

    for (_, key) in object_keys(file).unwrap() {
        assert!(exists_under(&w.served, &key), "{key} was not stored");
    }
    let image_url = &uploaded.urls.derivatives["image"];
    assert!(
        image_url.starts_with("https://img.example.com/files/"),
        "{image_url}"
    );
    assert!(uploaded.urls.derivatives.contains_key("thumb"));
    assert!(
        uploaded
            .urls
            .viewer
            .ends_with(&format!("/f/{}", file.id_encoded))
    );
    assert!(uploaded.urls.delete.is_some(), "a guest gets a delete link");
    assert_no_temps(&w);

    let storage = haloid_db::storages::list(&w.db).await.unwrap().remove(0);
    assert_eq!(storage.file_count, 1);

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_members_large_photo_gets_a_medium_and_is_counted_against_them() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool).await;
    let alice = user(&w.db, "alice", Role::User).await;

    let dir = TempDir::new().unwrap();
    let big = dir.path().join("big.jpg");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=900x600",
            "-frames:v",
            "1",
        ])
        .arg(&big)
        .status()
        .unwrap();
    assert!(status.success());
    let mut cmd = command(alice.actor(), "still.jpg");
    cmd.source = UploadSource::Stream {
        reader: Box::new(std::io::Cursor::new(std::fs::read(&big).unwrap())),
        filename: Some("big.jpg".into()),
    };

    let uploaded = w.uploads.upload(cmd).await.unwrap();
    assert_eq!(uploaded.file.chain.bits(), 7);
    assert!(uploaded.file.medium_size.is_some());
    let alice = haloid_db::users::find_by_id(&w.db, alice.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.file_count, 1);
    assert_eq!(alice.disk_used, uploaded.file.stored_bytes());

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_video_gets_a_frame_and_audio_gets_nothing_invented() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool).await;

    let video = w
        .uploads
        .upload(command(Actor::Guest, "clip.mp4"))
        .await
        .unwrap();
    assert_eq!(video.file.chain.bits(), 21);
    assert!(video.urls.derivatives["frame"].ends_with(".fr.jpg"));
    for (_, key) in object_keys(&video.file).unwrap() {
        assert!(exists_under(&w.served, &key), "{key}");
    }

    let audio = w
        .uploads
        .upload(command(Actor::Guest, "tone.mp3"))
        .await
        .unwrap();
    assert_eq!(audio.file.chain.bits(), 4);
    assert_eq!(audio.file.duration_ms.map(|ms| ms / 100), Some(10));

    scratch.drop_db().await;
}

#[tokio::test]
async fn audio_is_pro_and_refused_without_a_licence() {
    let scratch = migrated_or_skip!();
    let db = &scratch.pool;
    let w = world(db).await;
    let community = Licensing::load(db.clone(), Vec::new(), None).await.unwrap();
    assert!(!community.entitlements(Utc::now()).pro);
    let media = Arc::new(MediaToolkit::new(MediaConfig::default()));
    let settings = SettingsHandle::load(db.clone(), BTreeMap::new())
        .await
        .unwrap();
    let records = haloid_db::storages::list(db).await.unwrap();
    let pool = Arc::new(StoragePool::from_records(&records, w._dir.path(), &[]).unwrap());
    let uploads = Uploads::new(
        db.clone(),
        settings,
        media,
        pool,
        w._dir.path().join("tmp2"),
    )
    .with_licensing(Arc::new(community));

    let refused = uploads.upload(command(Actor::Guest, "tone.mp3")).await;
    assert!(
        matches!(refused, Err(DomainError::ProRequired("audio"))),
        "{refused:?}"
    );
    // Pictures and video are Community.
    uploads
        .upload(command(Actor::Guest, "clip.mp4"))
        .await
        .unwrap();

    scratch.drop_db().await;
}

/// Which error a refusal must produce.
type Expect = fn(&DomainError) -> bool;

#[tokio::test]
async fn every_refusal_leaves_nothing_behind() {
    let scratch = migrated_or_skip!();
    let w = world_with(
        &scratch.pool,
        &[
            ("uploads.max_size_guest", "2000"),
            ("uploads.stop_words", "forbidden"),
        ],
    )
    .await;

    let mut too_big = command(Actor::Guest, "tone.wav");
    too_big.title = None;
    let cases: Vec<(&str, UploadCommand, Expect)> = vec![
        ("over the size limit", too_big, |e| {
            matches!(e, DomainError::SizeLimit { limit: 2000, .. })
        }),
        ("an SVG", command(Actor::Guest, "xss.svg"), |e| {
            matches!(e, DomainError::Media(_))
        }),
        (
            "an SVG named .jpg",
            command(Actor::Guest, "svg-named.jpg"),
            |e| matches!(e, DomainError::Media(_)),
        ),
        (
            "a decompression bomb",
            command(Actor::Guest, "bomb.png"),
            |e| matches!(e, DomainError::PixelLimit { .. }),
        ),
        (
            "a stop word",
            UploadCommand {
                title: Some("Totally Forbidden".into()),
                ..command(Actor::Guest, "still.jpg")
            },
            |e| matches!(e, DomainError::StopWord),
        ),
        (
            "an empty body",
            UploadCommand {
                source: UploadSource::Stream {
                    reader: Box::new(std::io::Cursor::new(Vec::new())),
                    filename: None,
                },
                ..command(Actor::Guest, "still.jpg")
            },
            |e| matches!(e, DomainError::EmptySource),
        ),
        (
            "a URL source",
            UploadCommand {
                source: UploadSource::Url("http://169.254.169.254/".into()),
                ..command(Actor::Guest, "still.jpg")
            },
            |e| matches!(e, DomainError::UrlFetchRejected(_)),
        ),
        (
            "an unknown album",
            UploadCommand {
                album: Some("nope".into()),
                ..command(Actor::Guest, "still.jpg")
            },
            |e| matches!(e, DomainError::InvalidParam { .. }),
        ),
    ];
    for (what, cmd, expected) in cases {
        let error = w.uploads.upload(cmd).await.expect_err(what);
        assert!(expected(&error), "{what}: got {error:?}");
    }

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ih_files")
        .fetch_one(&w.db)
        .await
        .unwrap();
    assert_eq!(rows, 0, "a refused upload left a row");
    assert!(!w.served.exists() || std::fs::read_dir(&w.served).unwrap().next().is_none());
    assert_no_temps(&w);

    scratch.drop_db().await;
}

#[tokio::test]
async fn disallowed_types_and_disabled_guests_are_refused() {
    let scratch = migrated_or_skip!();
    let w = world_with(
        &scratch.pool,
        &[
            ("uploads.allowed_extensions", "jpg"),
            ("uploads.guests_enabled", "false"),
        ],
    )
    .await;
    let alice = user(&w.db, "alice", Role::User).await;

    assert!(matches!(
        w.uploads.upload(command(Actor::Guest, "still.jpg")).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        w.uploads.upload(command(alice.actor(), "still.png")).await,
        Err(DomainError::ExtensionNotAllowed(e)) if e == "png"
    ));
    assert!(
        w.uploads
            .upload(command(alice.actor(), "still.jpg"))
            .await
            .is_ok()
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn base64_sources_work_with_a_data_prefix() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool).await;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD
        .encode(std::fs::read(fixture("still.png")).unwrap());
    let mut cmd = command(Actor::Guest, "still.png");
    cmd.source = UploadSource::Base64 {
        data: format!("data:image/png;base64,{encoded}\n"),
        filename: None,
    };
    let uploaded = w.uploads.upload(cmd).await.unwrap();
    assert_eq!(uploaded.file.extension, "png");
    assert!(
        uploaded.file.name.starts_with("file-"),
        "nameless uploads are 'file'"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn duplicates_return_the_existing_upload() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool).await;
    let alice = user(&w.db, "alice", Role::User).await;

    let first = w
        .uploads
        .upload(command(alice.actor(), "still.jpg"))
        .await
        .unwrap();
    let again = w
        .uploads
        .upload(command(alice.actor(), "still.jpg"))
        .await
        .unwrap();
    assert!(again.duplicate);
    assert_eq!(again.file.id, first.file.id);
    assert_eq!(
        again.urls.delete, first.urls.delete,
        "the owner keeps their delete link"
    );

    let guest_first = w
        .uploads
        .upload(command(Actor::Guest, "still.png"))
        .await
        .unwrap();
    let guest_again = w
        .uploads
        .upload(command(Actor::Guest, "still.png"))
        .await
        .unwrap();
    assert_eq!(guest_again.file.id, guest_first.file.id);
    assert_eq!(
        guest_again.urls.delete, None,
        "another guest must not get a way to delete it"
    );

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ih_files")
        .fetch_one(&w.db)
        .await
        .unwrap();
    assert_eq!(rows, 2);

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_width_resize_keeps_the_original_and_sets_bit_eight() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool).await;
    let mut cmd = command(Actor::Guest, "still.jpg");
    cmd.width = Some(10);

    let uploaded = w.uploads.upload(cmd).await.unwrap();
    let file = &uploaded.file;
    assert_eq!((file.width, file.height), (10, 5));
    assert!(file.chain.contains(Derivative::Original));
    assert!(!file.chain.legacy().contains(Derivative::Original));
    assert_eq!(file.original_extension.as_deref(), Some("jpg"));
    assert_ne!(Some(&file.checksum), file.source_checksum.as_ref());
    let keys = object_keys(file).unwrap();
    let original = keys
        .iter()
        .find(|(d, _)| *d == Derivative::Original)
        .unwrap();
    assert!(original.1.ends_with(".or.jpg"));
    assert!(exists_under(&w.served, &original.1));

    scratch.drop_db().await;
}

#[tokio::test]
async fn stripping_removes_identifying_metadata_but_keeps_display_exif() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool).await;
    let uploaded = w
        .uploads
        .upload(command(Actor::Guest, "exif.jpg"))
        .await
        .unwrap();

    let image_key = &uploaded.file.storage_path;
    let stored = std::fs::read(w.served.join(image_key)).unwrap();
    assert!(
        !stored.windows(9).any(|win| win == b"HaloidCam"),
        "EXIF survived"
    );
    assert_eq!(
        (uploaded.file.width, uploaded.file.height),
        (16, 32),
        "still rotated"
    );

    let exif = haloid_db::files::exif(&w.db, uploaded.file.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        exif.contains("HaloidCam"),
        "the viewer's EXIF was captured first"
    );
    assert!(!exif.contains("GPS"));

    scratch.drop_db().await;
}

#[tokio::test]
async fn moderation_tags_albums_and_expiry_are_applied() {
    let scratch = migrated_or_skip!();
    let w = world_with(&scratch.pool, &[("moderation.mode", "all")]).await;
    let alice = user(&w.db, "alice", Role::User).await;
    let bob = user(&w.db, "bob", Role::User).await;
    let album = haloid_db::albums::create(
        &w.db,
        &haloid_db::albums::NewAlbum {
            user_id: Some(alice.id),
            parent_id: None,
            name: "Cats",
            description: None,
            privacy: AlbumPrivacy::Public,
            password_hash: None,
        },
    )
    .await
    .unwrap();

    let mut cmd = command(alice.actor(), "still.jpg");
    cmd.tags = vec!["Cats".into(), "black and white".into()];
    cmd.album = Some(album.id_encoded.to_string());
    cmd.expiration = Some(Duration::from_secs(3600));
    let uploaded = w.uploads.upload(cmd).await.unwrap();

    assert!(!uploaded.file.is_approved, "held for moderation");
    assert_eq!(
        haloid_db::moderation::pending_count(&w.db).await.unwrap(),
        1
    );
    for (_, key) in object_keys(&uploaded.file).unwrap() {
        assert!(
            !exists_under(&w.served, &key),
            "{key} is served before a moderator approved it"
        );
        assert!(exists_under(&w.quarantine, &key), "{key} was not kept");
    }
    let slugs: Vec<&str> = uploaded.tags.iter().map(|t| t.slug.as_str()).collect();
    assert_eq!(slugs, vec!["cats", "black-and-white"], "as written");
    assert_eq!(uploaded.album.map(|a| a.id), Some(album.id));
    let expires = uploaded.file.expires_at.expect("expiry set");
    let delta = (expires - Utc::now()).num_seconds();
    assert!((3500..=3600).contains(&delta), "{delta}");

    let mut other = command(bob.actor(), "still.png");
    other.album = Some(album.id_encoded.to_string());
    assert!(matches!(
        w.uploads.upload(other).await,
        Err(DomainError::InvalidParam { .. })
    ));

    let admin = user(&w.db, "root2", Role::Admin).await;
    let unmoderated = w
        .uploads
        .upload(command(admin.actor(), "still.gif"))
        .await
        .unwrap();
    assert!(unmoderated.file.is_approved, "moderators skip the queue");

    scratch.drop_db().await;
}

#[tokio::test]
async fn deleting_quarantines_the_bytes_and_only_an_admin_restores_or_purges() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool).await;
    let alice = user(&w.db, "alice", Role::User).await;
    let bob = user(&w.db, "bob", Role::User).await;
    let admin = user(&w.db, "boss", Role::Admin).await;
    let uploaded = w
        .uploads
        .upload(command(alice.actor(), "clip.mp4"))
        .await
        .unwrap();
    let id = uploaded.file.id_encoded.to_string();
    let keys = object_keys(&uploaded.file).unwrap();

    assert!(matches!(
        w.files
            .delete(&id, DeleteAs::Actor(bob.actor()), Utc::now())
            .await,
        Err(DomainError::NotFound(_))
    ));
    w.files
        .delete(&id, DeleteAs::Actor(alice.actor()), Utc::now())
        .await
        .unwrap();
    for (_, key) in &keys {
        assert!(!exists_under(&w.served, key), "{key} is still served");
        assert!(exists_under(&w.quarantine, key), "{key} was not retained");
    }
    let row = haloid_db::files::find_by_id(&w.db, uploaded.file.id)
        .await
        .unwrap()
        .unwrap();
    let deletion = row.deletion.unwrap();
    assert_eq!(
        (deletion.by, deletion.reason),
        (Some(alice.id), DeleteReason::Owner)
    );
    // Running it again is harmless: the quarantine is idempotent.
    w.files
        .delete(&id, DeleteAs::Actor(alice.actor()), Utc::now())
        .await
        .unwrap();

    assert!(matches!(
        w.files.restore(alice.actor(), &id).await,
        Err(DomainError::Forbidden)
    ));
    w.files.restore(admin.actor(), &id).await.unwrap();
    for (_, key) in &keys {
        assert!(exists_under(&w.served, key), "{key} was not released");
    }

    assert!(matches!(
        w.files.purge(alice.actor(), &id).await,
        Err(DomainError::Forbidden)
    ));
    w.files.purge(admin.actor(), &id).await.unwrap();
    for (_, key) in &keys {
        assert!(!exists_under(&w.served, key) && !exists_under(&w.quarantine, key));
    }
    assert!(
        haloid_db::files::find_by_id(&w.db, uploaded.file.id)
            .await
            .unwrap()
            .is_none()
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_guest_deletes_with_the_token_and_nothing_else() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool).await;
    let uploaded = w
        .uploads
        .upload(command(Actor::Guest, "still.jpg"))
        .await
        .unwrap();
    let id = uploaded.file.id_encoded.to_string();
    let token = uploaded
        .urls
        .delete
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();

    assert!(
        w.files
            .delete(&id, DeleteAs::Token("0".repeat(32).as_str()), Utc::now())
            .await
            .is_err()
    );
    assert!(
        w.files
            .delete(&id, DeleteAs::Actor(Actor::Guest), Utc::now())
            .await
            .is_err(),
        "being a guest is not ownership"
    );
    w.files
        .delete(&id, DeleteAs::Token(&token), Utc::now())
        .await
        .unwrap();
    let row = haloid_db::files::find_by_id(&w.db, uploaded.file.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.deletion.unwrap().reason, DeleteReason::DeleteToken);

    scratch.drop_db().await;
}
