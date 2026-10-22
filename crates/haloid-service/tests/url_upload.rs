//! Uploading from a URL, against a mock server. The fetcher here may reach
//! loopback, where the mock listens, and nothing else private — so the
//! refusal of a redirect into the private network is real.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use haloid_core::DomainError;
use haloid_core::auth::Actor;
use haloid_core::storage::{StorageKind, StoragePathMode};
use haloid_media::{MediaConfig, MediaToolkit};
use haloid_service::fetch::Reach;
use haloid_service::settings::SettingsHandle;
use haloid_service::upload::{UploadCommand, UploadSource, Uploads};
use haloid_storage::pool::StoragePool;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn uploads(db: &haloid_db::Db, dir: &TempDir, allow: bool) -> Uploads {
    haloid_db::storages::create(
        db,
        &haloid_db::storages::StorageDetails {
            name: "main".into(),
            kind: StorageKind::Local,
            config: "{}".into(),
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
    settings
        .update(&BTreeMap::from([
            ("uploads.allow_url_sources".to_string(), allow.to_string()),
            ("uploads.max_size_guest".to_string(), "20000".to_string()),
        ]))
        .await
        .unwrap();
    Uploads::new(
        db.clone(),
        settings,
        Arc::new(MediaToolkit::new(MediaConfig::default())),
        pool,
        dir.path().join("tmp"),
    )
    .with_url_reach(Reach::PublicAndLoopback)
}

fn from(url: String) -> UploadCommand {
    UploadCommand {
        actor: Actor::Guest,
        source: UploadSource::Url(url),
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

fn png() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/still.png"))
        .unwrap()
}

#[tokio::test]
async fn a_picture_at_a_url_is_uploaded_following_redirects() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let uploads = uploads(&scratch.pool, &dir, true).await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pictures/sunset%20sky.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(png())
                .insert_header("content-type", "image/png"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/short"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "/pictures/sunset%20sky.png"),
        )
        .mount(&server)
        .await;

    let uploaded = uploads
        .upload(from(format!("{}/short", server.uri())))
        .await
        .unwrap();
    assert_eq!(uploaded.file.width, 32);
    assert_eq!(
        uploaded.file.original_filename.as_deref(),
        Some("sunset sky.png"),
        "named after the final URL"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn refusals_are_firm() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let uploads = uploads(&scratch.pool, &dir, true).await;
    let server = MockServer::start().await;
    // Redirects into the private network, endlessly, and to a 404.
    Mock::given(path("/to-metadata"))
        .respond_with(
            ResponseTemplate::new(301)
                .insert_header("location", "http://169.254.169.254/latest/meta-data/"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/to-lan"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", "http://10.0.0.7/admin"))
        .mount(&server)
        .await;
    Mock::given(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop"))
        .mount(&server)
        .await;
    Mock::given(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    // Bigger than the 20 000-byte guest limit, declared and not.
    Mock::given(path("/declared-big"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 50_000]))
        .mount(&server)
        .await;

    for (url, what) in [
        ("/to-metadata", "a redirect to the metadata endpoint"),
        ("/to-lan", "a redirect into the private network"),
        ("/loop", "a redirect loop"),
        ("/missing", "a 404"),
    ] {
        let error = uploads
            .upload(from(format!("{}{url}", server.uri())))
            .await
            .err()
            .unwrap();
        assert!(
            matches!(error, DomainError::UrlFetchRejected(_)),
            "{what}: {error:?}"
        );
    }
    let error = uploads
        .upload(from(format!("{}/declared-big", server.uri())))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, DomainError::SizeLimit { .. }), "{error:?}");

    // Off: refused before any request.
    let off_dir = TempDir::new().unwrap();
    let scratch_off = migrated_or_skip!();
    let off = self::uploads(&scratch_off.pool, &off_dir, false).await;
    let error = off
        .upload(from(format!("{}/missing", server.uri())))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, DomainError::UrlFetchRejected(_)));

    scratch_off.drop_db().await;
    scratch.drop_db().await;
}
