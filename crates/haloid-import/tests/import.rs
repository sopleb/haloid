//! Importing the fixture Chevereto database into a fresh Haloid one.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use haloid_import::legacy_id::LegacyIds;
use haloid_import::{Options, run};
use sqlx::{Executor, MySqlPool, Row};
use std::path::Path;

/// A scratch database holding the fixture Chevereto tables.
async fn upstream() -> Option<(MySqlPool, MySqlPool, String)> {
    let base = std::env::var("TEST_DATABASE_URL").ok()?;
    let base = base.trim_end_matches('/').to_string();
    let name = format!("haloid_upstream_{}", uuid::Uuid::new_v4().simple());
    let server = MySqlPool::connect(&base).await.unwrap();
    server
        .execute(format!("CREATE DATABASE `{name}`").as_str())
        .await
        .unwrap();
    let db = MySqlPool::connect(&format!("{base}/{name}")).await.unwrap();
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/import/upstream.sql"),
    )
    .unwrap();
    for statement in fixture
        .split(";\n")
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s| !s.trim().is_empty())
    {
        db.execute(statement.as_str()).await.unwrap();
    }
    Some((server, db, name))
}

fn options() -> Options {
    Options {
        files_dir: Some("/srv/chevereto/images".into()),
        files_url: Some("https://old.example.com/images".into()),
        ..Options::default()
    }
}

#[tokio::test]
async fn a_chevereto_database_comes_across_with_its_ids_and_links() {
    let scratch = migrated_or_skip!();
    let target = &scratch.pool;
    let Some((server, source, name)) = upstream().await else {
        return;
    };
    let ids = LegacyIds::new("salty", 0);

    // A dry run reports and writes nothing.
    let dry = run(
        &source,
        target,
        &Options {
            dry_run: true,
            ..options()
        },
    )
    .await
    .unwrap();
    assert_eq!((dry.users, dry.albums, dry.files), (3, 3, 4));
    let count = |table: &'static str| async move {
        sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(target)
            .await
            .unwrap()
    };
    assert_eq!(count("ih_users").await, 0);

    let report = run(&source, target, &options()).await.unwrap();
    assert_eq!(report.users, 3);
    assert_eq!(report.albums, 3);
    // The PDF is not a kind Haloid takes, and the FTP storage's file has
    // no storage to go to.
    assert_eq!(report.files, 4, "{report:?}");
    assert!(report.skipped.iter().any(|s| s.contains(".pdf")));
    assert!(report.skipped.iter().any(|s| s.contains("ftp")));
    assert!(
        report
            .todo
            .iter()
            .any(|t| t.contains("HALOID_STORAGE_IMPORTED_1_ACCESS_KEY"))
    );

    // Users: roles, statuses, the newest bcrypt hash, the old public id.
    let mira = sqlx::query(
        "SELECT role, status, email_lower, password_hash, is_private, legacy_id_encoded, timezone
         FROM ih_users WHERE username = 'mira'",
    )
    .fetch_one(target)
    .await
    .unwrap();
    assert_eq!(mira.get::<String, _>("role"), "user");
    assert_eq!(mira.get::<String, _>("status"), "awaiting_confirmation");
    assert_eq!(mira.get::<String, _>("email_lower"), "mira@example.com");
    assert!(
        mira.get::<String, _>("password_hash")
            .starts_with("$2y$04$")
    );
    assert!(!mira.get::<String, _>("password_hash").contains("oldold"));
    assert!(mira.get::<bool, _>("is_private"));
    assert_eq!(mira.get::<String, _>("legacy_id_encoded"), ids.encode(2));
    let admin_role: String =
        sqlx::query_scalar("SELECT role FROM ih_users WHERE username = 'admin'")
            .fetch_one(target)
            .await
            .unwrap();
    assert_eq!(admin_role, "admin");

    // Albums: privacy mapped, the password hashed, the parent found though
    // it came later, the cover set.
    let album = |legacy: u64| {
        let legacy = ids.encode(legacy);
        async move {
            sqlx::query(
                "SELECT a.privacy, a.password_hash, p.name AS parent, a.cover_file_id, a.file_count, a.view_count
                 FROM ih_albums a LEFT JOIN ih_albums p ON p.id = a.parent_id WHERE a.legacy_id_encoded = ?",
            )
            .bind(legacy)
            .fetch_one(target)
            .await
            .unwrap()
        }
    };
    let trips = album(1).await;
    assert_eq!(trips.get::<String, _>("privacy"), "unlisted");
    assert_eq!(
        trips.get::<Option<String>, _>("parent").as_deref(),
        Some("Everything")
    );
    assert_eq!(trips.get::<u32, _>("file_count"), 2);
    assert_eq!(trips.get::<u64, _>("view_count"), 12);
    let secret = album(3).await;
    assert_eq!(secret.get::<String, _>("privacy"), "password");
    let hash: String = secret.get("password_hash");
    assert!(haloid_service::auth::secrets::verify_password(
        "open sesame",
        &hash
    ));

    // Files: where they sit, their kind and counts, the old id.
    let sunset = sqlx::query(
        "SELECT kind, storage_path, storage_mode, view_count, like_count, title, exif, uploader_ip, category_id,
                chain, album_position, legacy_id_encoded, state
         FROM ih_files WHERE name = 'sunset-Ab12'",
    )
    .fetch_one(target)
    .await
    .unwrap();
    assert_eq!(sunset.get::<String, _>("kind"), "image");
    assert_eq!(
        sunset.get::<String, _>("storage_path"),
        "2021/06/02/sunset-Ab12.jpg",
        "the local date's folder"
    );
    assert_eq!(sunset.get::<String, _>("state"), "ready");
    assert_eq!(sunset.get::<u64, _>("view_count"), 42);
    assert_eq!(sunset.get::<u32, _>("like_count"), 1);
    assert_eq!(sunset.get::<u8, _>("chain"), 7);
    assert!(sunset.get::<Option<u32>, _>("category_id").is_some());
    assert_eq!(
        sunset
            .get::<Option<Vec<u8>>, _>("uploader_ip")
            .map(|b| b.len()),
        Some(4)
    );
    assert_eq!(sunset.get::<String, _>("legacy_id_encoded"), ids.encode(1));
    let harbour_exif: Option<String> =
        sqlx::query_scalar("SELECT CAST(exif AS CHAR) FROM ih_files WHERE name = 'harbour-Cd34'")
            .fetch_one(target)
            .await
            .unwrap();
    assert_eq!(harbour_exif, None, "unreadable EXIF is dropped, not stored");
    let clip = sqlx::query(
        "SELECT kind, duration_ms, storage_id, expires_at FROM ih_files WHERE name = 'clip-Ef56'",
    )
    .fetch_one(target)
    .await
    .unwrap();
    assert_eq!(clip.get::<String, _>("kind"), "video");
    assert_eq!(clip.get::<Option<u32>, _>("duration_ms"), Some(12_000));
    let guest = sqlx::query("SELECT user_id, storage_path FROM ih_files WHERE name = 'guest-Kl12'")
        .fetch_one(target)
        .await
        .unwrap();
    assert_eq!(guest.get::<Option<u64>, _>("user_id"), None);
    assert_eq!(guest.get::<String, _>("storage_path"), "guest-Kl12.webp");

    // Tags in the order they were linked; storages; the user's counts.
    let tags: Vec<String> = sqlx::query_scalar(
        "SELECT t.name FROM ih_tags t JOIN ih_file_tags ft ON ft.tag_id = t.id
         JOIN ih_files f ON f.id = ft.file_id WHERE f.name = 'sunset-Ab12' ORDER BY ft.position",
    )
    .fetch_all(target)
    .await
    .unwrap();
    assert_eq!(tags, ["sunset", "bay"]);
    let storages: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT name, CAST(kind AS CHAR), url_prefix, is_active FROM ih_storages ORDER BY id",
    )
    .fetch_all(target)
    .await
    .unwrap();
    assert!(storages.contains(&(
        "imported".into(),
        "local".into(),
        "https://old.example.com/images".into(),
        false
    )));
    assert!(storages.iter().any(|s| s.0 == "imported-1" && s.1 == "s3"));
    let (files, albums): (u32, u32) =
        sqlx::query_as("SELECT file_count, album_count FROM ih_users WHERE username = 'mira'")
            .fetch_one(target)
            .await
            .unwrap();
    assert_eq!((files, albums), (2, 3));

    // Again: everything is there already, nothing doubles.
    let again = run(&source, target, &options()).await.unwrap();
    assert_eq!((again.users, again.albums, again.files), (0, 0, 0));
    assert!(again.already >= 10, "{again:?}");
    assert_eq!(count("ih_files").await, 4);

    let _ = server
        .execute(format!("DROP DATABASE `{name}`").as_str())
        .await;
    scratch.drop_db().await;
}

#[tokio::test]
async fn files_in_chevereto_s_own_folder_need_its_path() {
    let scratch = migrated_or_skip!();
    let Some((server, source, name)) = upstream().await else {
        return;
    };
    let error = run(&source, &scratch.pool, &Options::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("--files-dir"), "{error}");
    let _ = server
        .execute(format!("DROP DATABASE `{name}`").as_str())
        .await;
    scratch.drop_db().await;
}
