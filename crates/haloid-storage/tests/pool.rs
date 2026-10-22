//! Building the pool of storages from their configuration rows.

use chrono::DateTime;
use haloid_core::StorageError;
use haloid_core::ids::StorageId;
use haloid_core::storage::{PutBody, StorageKind, StoragePathMode, StorageRecord};
use haloid_storage::keys::Layout;
use haloid_storage::pool::StoragePool;
use tempfile::TempDir;

fn record(id: u64, name: &str, config: &str) -> StorageRecord {
    StorageRecord {
        id: StorageId(id),
        name: name.into(),
        kind: StorageKind::Local,
        config: config.into(),
        url_prefix: format!("https://img.example.com/{name}"),
        path_mode: StoragePathMode::DateFolder,
        path_template: None,
        capacity_bytes: None,
        used_bytes: 0,
        file_count: 0,
        is_active: true,
        priority: 0,
        created_at: DateTime::UNIX_EPOCH,
        updated_at: DateTime::UNIX_EPOCH,
        retired_at: None,
    }
}

#[tokio::test]
async fn a_local_storage_defaults_its_directories_under_the_data_dir() {
    let data = TempDir::new().unwrap();
    let pool = StoragePool::from_records(&[record(1, "main", "{}")], data.path(), &[]).unwrap();

    let storage = pool.get(StorageId(1)).expect("built");
    storage
        .put("a.jpg", PutBody::Bytes(vec![1, 2, 3]), "image/jpeg")
        .await
        .unwrap();
    assert!(data.path().join("storage/main/a.jpg").exists());

    storage.quarantine("a.jpg").await.unwrap();
    assert!(data.path().join("quarantine/main/a.jpg").exists());

    assert_eq!(pool.layout(StorageId(1)), Some(Layout::DateFolder));
    assert!(pool.get(StorageId(2)).is_none());
}

#[tokio::test]
async fn configured_directories_are_honoured_and_relative_ones_sit_in_the_data_dir() {
    let data = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let config = format!(
        r#"{{"base_path": {:?}, "quarantine_path": "held/main"}}"#,
        elsewhere.path().join("served").display().to_string()
    );
    let pool = StoragePool::from_records(&[record(1, "main", &config)], data.path(), &[]).unwrap();

    let storage = pool.get(StorageId(1)).unwrap();
    storage
        .put("a.jpg", PutBody::Bytes(vec![1]), "image/jpeg")
        .await
        .unwrap();
    assert!(elsewhere.path().join("served/a.jpg").exists());
    storage.quarantine("a.jpg").await.unwrap();
    assert!(data.path().join("held/main/a.jpg").exists());
}

#[test]
fn templates_are_parsed_once_at_load() {
    let data = TempDir::new().unwrap();
    let mut templated = record(1, "main", "{}");
    templated.path_mode = StoragePathMode::Template;
    templated.path_template = Some("{user_id}/{yyyy}".into());
    let pool = StoragePool::from_records(&[templated], data.path(), &[]).unwrap();
    assert!(matches!(
        pool.layout(StorageId(1)),
        Some(Layout::Template(_))
    ));
}

#[test]
fn a_broken_configuration_is_refused_and_names_the_storage() {
    let data = TempDir::new().unwrap();

    let mut no_template = record(1, "tpl", "{}");
    no_template.path_mode = StoragePathMode::Template;

    let mut bad_template = record(2, "badtpl", "{}");
    bad_template.path_mode = StoragePathMode::Template;
    bad_template.path_template = Some("../{yyyy}".into());

    for broken in [
        record(4, "notjson", "not json"),
        record(5, "typo", r#"{"base_pth": "/srv"}"#),
        record(6, "../escape", "{}"),
        no_template,
        bad_template,
    ] {
        let name = broken.name.clone();
        match StoragePool::from_records(&[broken], data.path(), &[]) {
            Err(StorageError::Misconfigured(message)) => {
                assert!(message.contains(&name), "{message:?} does not name {name}")
            }
            Err(other) => panic!("{name}: expected Misconfigured, got {other:?}"),
            Ok(_) => panic!("{name}: accepted"),
        }
    }
}

#[test]
fn retired_storages_stay_in_the_pool_to_keep_serving() {
    let data = TempDir::new().unwrap();
    let mut retired = record(1, "old", "{}");
    retired.retired_at = Some(DateTime::UNIX_EPOCH);
    retired.is_active = false;
    let pool = StoragePool::from_records(&[retired], data.path(), &[]).unwrap();
    assert!(pool.get(StorageId(1)).is_some());
}

/// A stand-in for the Pro build's S3 backend: builds a local storage, so
/// the pool's dispatch can be tested without the Pro code.
struct FakeS3(std::path::PathBuf);

impl haloid_storage::backend::StorageBackend for FakeS3 {
    fn kind(&self) -> StorageKind {
        StorageKind::S3
    }
    fn build(
        &self,
        record: &StorageRecord,
        env: haloid_storage::backend::EnvLookup<'_>,
    ) -> Result<std::sync::Arc<dyn haloid_core::storage::Storage>, StorageError> {
        let (access, _) = haloid_storage::backend::credential_vars(&record.name);
        assert_eq!(
            env(&access).as_deref(),
            Some("key"),
            "credentials come through env"
        );
        Ok(std::sync::Arc::new(
            haloid_storage::local::LocalStorage::new(
                record.id,
                &record.url_prefix,
                &self.0.join("served"),
                &self.0.join("quarantine"),
            )
            .unwrap(),
        ))
    }
    fn check_config(&self, _config: &str) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn a_kind_without_a_backend_is_left_out_and_named() {
    let data = TempDir::new().unwrap();
    let mut s3 = record(2, "bucket", "{}");
    s3.kind = StorageKind::S3;
    let pool = StoragePool::from_records(&[record(1, "main", "{}"), s3], data.path(), &[]).unwrap();
    assert!(pool.get(StorageId(1)).is_some());
    assert!(pool.get(StorageId(2)).is_none());
    assert_eq!(pool.unavailable(), ["bucket"]);
    assert_eq!(pool.kinds(), [StorageKind::Local]);
}

#[tokio::test]
async fn a_backend_builds_the_storages_of_its_kind() {
    let data = TempDir::new().unwrap();
    let mut s3 = record(2, "bucket", "{}");
    s3.kind = StorageKind::S3;
    let backends: Vec<std::sync::Arc<dyn haloid_storage::backend::StorageBackend>> =
        vec![std::sync::Arc::new(FakeS3(data.path().join("fake")))];
    let pool = StoragePool::from_records_with_env(&[s3], data.path(), &backends, &|name| {
        (name == "HALOID_STORAGE_BUCKET_ACCESS_KEY").then(|| "key".to_string())
    })
    .unwrap();
    assert!(pool.get(StorageId(2)).is_some());
    assert!(pool.unavailable().is_empty());
    assert_eq!(pool.kinds(), [StorageKind::Local, StorageKind::S3]);
}
