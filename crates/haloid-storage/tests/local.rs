//! Local filesystem storage, against a real temporary directory.

use haloid_core::StorageError;
use haloid_core::ids::StorageId;
use haloid_core::storage::{PutBody, Storage, StorageKind};
use haloid_storage::local::LocalStorage;
use std::path::Path;
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    root: std::path::PathBuf,
    quarantine: std::path::PathBuf,
    storage: LocalStorage,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("public");
    let quarantine = dir.path().join("quarantine");
    let storage = LocalStorage::new(
        StorageId(1),
        "https://img.example.com/files/",
        &root,
        &quarantine,
    )
    .expect("a valid local storage");
    Fixture {
        _dir: dir,
        root,
        quarantine,
        storage,
    }
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[tokio::test]
async fn bytes_are_written_under_the_root_and_described() {
    let f = fixture();
    let meta = f
        .storage
        .put(
            "2026/10/21/cat.jpg",
            PutBody::Bytes(b"meow".to_vec()),
            "image/jpeg",
        )
        .await
        .unwrap();
    assert_eq!(meta.size, 4);
    assert_eq!(read(&f.root.join("2026/10/21/cat.jpg")), b"meow");

    let head = f.storage.head("2026/10/21/cat.jpg").await.unwrap();
    assert_eq!(head.size, 4);
    assert_eq!(f.storage.kind(), StorageKind::Local);
    assert_eq!(f.storage.id(), StorageId(1));
}

#[tokio::test]
async fn a_temp_file_is_moved_into_place_not_copied_through_memory() {
    let f = fixture();
    let upload = f.root.parent().unwrap().join("upload.tmp");
    std::fs::write(&upload, vec![7u8; 10_000]).unwrap();

    f.storage
        .put("v/clip.mp4", PutBody::Path(upload.clone()), "video/mp4")
        .await
        .unwrap();
    assert!(!upload.exists(), "the temp file was left behind");
    assert_eq!(read(&f.root.join("v/clip.mp4")).len(), 10_000);
}

#[tokio::test]
async fn overwriting_replaces_the_object_whole() {
    let f = fixture();
    for body in [&b"first version"[..], b"second"] {
        f.storage
            .put("a.txt", PutBody::Bytes(body.to_vec()), "text/plain")
            .await
            .unwrap();
    }
    assert_eq!(read(&f.root.join("a.txt")), b"second");
    let leftovers: Vec<_> = std::fs::read_dir(&f.root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, vec!["a.txt"], "a temp file was left beside it");
}

#[tokio::test]
async fn urls_are_built_from_the_prefix_and_percent_encoded() {
    let f = fixture();
    assert_eq!(
        f.storage.url_for("2026/10/21/cat.jpg"),
        "https://img.example.com/files/2026/10/21/cat.jpg",
        "a trailing slash on the prefix is not doubled"
    );
    assert_eq!(
        f.storage.url_for("猫/a b#c?.jpg"),
        "https://img.example.com/files/%E7%8C%AB/a%20b%23c%3F.jpg"
    );
}

#[tokio::test]
async fn deleting_is_idempotent() {
    let f = fixture();
    f.storage
        .put("a.jpg", PutBody::Bytes(vec![1]), "image/jpeg")
        .await
        .unwrap();
    f.storage.delete("a.jpg").await.unwrap();
    assert!(!f.root.join("a.jpg").exists());
    f.storage
        .delete("a.jpg")
        .await
        .expect("deleting a missing object succeeds");
    assert!(matches!(
        f.storage.head("a.jpg").await,
        Err(StorageError::NotFound(_))
    ));
}

#[tokio::test]
async fn delete_many_reports_only_the_failures() {
    let f = fixture();
    for key in ["a.jpg", "b.jpg"] {
        f.storage
            .put(key, PutBody::Bytes(vec![1]), "image/jpeg")
            .await
            .unwrap();
    }
    let failed = f
        .storage
        .delete_many(&["a.jpg".into(), "../escape".into(), "b.jpg".into()])
        .await
        .unwrap();
    assert_eq!(failed, vec!["../escape".to_string()]);
    assert!(!f.root.join("a.jpg").exists() && !f.root.join("b.jpg").exists());
}

#[tokio::test]
async fn quarantine_takes_an_object_out_of_the_served_tree_and_back() {
    let f = fixture();
    f.storage
        .put(
            "2026/cat.jpg",
            PutBody::Bytes(b"meow".to_vec()),
            "image/jpeg",
        )
        .await
        .unwrap();

    f.storage.quarantine("2026/cat.jpg").await.unwrap();
    assert!(
        !f.root.join("2026/cat.jpg").exists(),
        "a quarantined object is still being served"
    );
    assert_eq!(read(&f.quarantine.join("2026/cat.jpg")), b"meow");
    f.storage
        .quarantine("2026/cat.jpg")
        .await
        .expect("quarantining twice succeeds, so a batch can be retried");

    f.storage.release("2026/cat.jpg").await.unwrap();
    assert_eq!(read(&f.root.join("2026/cat.jpg")), b"meow");
    assert!(!f.quarantine.join("2026/cat.jpg").exists());
    f.storage
        .release("2026/cat.jpg")
        .await
        .expect("releasing twice succeeds");
}

#[tokio::test]
async fn a_quarantined_object_is_purged_from_quarantine() {
    let f = fixture();
    f.storage
        .put("cat.jpg", PutBody::Bytes(b"meow".to_vec()), "image/jpeg")
        .await
        .unwrap();
    f.storage.quarantine("cat.jpg").await.unwrap();

    f.storage.delete_quarantined("cat.jpg").await.unwrap();
    assert!(!f.quarantine.join("cat.jpg").exists());
    f.storage
        .delete_quarantined("cat.jpg")
        .await
        .expect("purging twice succeeds");
}

#[tokio::test]
async fn moving_an_object_found_nowhere_is_not_found() {
    let f = fixture();
    assert!(matches!(
        f.storage.quarantine("ghost.jpg").await,
        Err(StorageError::NotFound(_))
    ));
    assert!(matches!(
        f.storage.release("ghost.jpg").await,
        Err(StorageError::NotFound(_))
    ));
}

#[tokio::test]
async fn every_operation_refuses_an_escaping_key() {
    let f = fixture();
    let outside = f.root.parent().unwrap().join("secret.txt");
    std::fs::write(&outside, b"do not touch").unwrap();

    for key in ["../secret.txt", "/etc/passwd", "a/../../secret.txt", ""] {
        let invalid = |r: Result<(), StorageError>| matches!(r, Err(StorageError::InvalidKey(_)));
        assert!(
            invalid(
                f.storage
                    .put(key, PutBody::Bytes(vec![1]), "x/y")
                    .await
                    .map(|_| ())
            ),
            "put {key:?}"
        );
        assert!(
            invalid(f.storage.head(key).await.map(|_| ())),
            "head {key:?}"
        );
        assert!(invalid(f.storage.delete(key).await), "delete {key:?}");
        assert!(
            invalid(f.storage.quarantine(key).await),
            "quarantine {key:?}"
        );
        assert!(invalid(f.storage.release(key).await), "release {key:?}");
        assert!(
            invalid(f.storage.delete_quarantined(key).await),
            "delete_quarantined {key:?}"
        );
    }
    assert_eq!(read(&outside), b"do not touch");
}

#[tokio::test]
async fn usage_counts_what_is_served_not_what_is_quarantined() {
    let f = fixture();
    for (key, size) in [("a.jpg", 10usize), ("d/b.jpg", 20), ("d/e/c.jpg", 30)] {
        f.storage
            .put(key, PutBody::Bytes(vec![0; size]), "image/jpeg")
            .await
            .unwrap();
    }
    f.storage.quarantine("a.jpg").await.unwrap();

    let usage = f.storage.usage().await.unwrap();
    assert_eq!((usage.bytes, usage.objects), (50, 2));
}

#[test]
fn a_quarantine_inside_the_served_tree_is_refused() {
    // It would be served, which is the one thing quarantine must not be.
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("public");
    for quarantine in [
        root.join("quarantine"),
        root.clone(),
        dir.path().to_path_buf(),
    ] {
        assert!(
            matches!(
                LocalStorage::new(StorageId(1), "http://x", &root, &quarantine),
                Err(StorageError::Misconfigured(_))
            ),
            "{} accepted",
            quarantine.display()
        );
    }
}
