//! An imported Chevereto's old page links, redirected.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig};
use sqlx::{Executor, MySqlPool};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

#[tokio::test]
async fn old_image_and_album_links_go_to_their_new_pages() {
    let scratch = migrated_or_skip!();
    let base = std::env::var("TEST_DATABASE_URL").unwrap();
    let base = base.trim_end_matches('/');
    let name = format!("haloid_upstream_{}", uuid::Uuid::new_v4().simple());
    let server = MySqlPool::connect(base).await.unwrap();
    server
        .execute(format!("CREATE DATABASE `{name}`").as_str())
        .await
        .unwrap();
    let source = MySqlPool::connect(&format!("{base}/{name}")).await.unwrap();
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
        source.execute(statement.as_str()).await.unwrap();
    }
    let dir = TempDir::new().unwrap();
    haloid_import::run(
        &source,
        &scratch.pool,
        &haloid_import::Options {
            files_dir: Some(dir.path().join("images")),
            files_url: Some("http://localhost:8080/images".into()),
            ..haloid_import::Options::default()
        },
    )
    .await
    .unwrap();
    let app = Arc::new(
        App::start_default(
            scratch.pool.clone(),
            AppConfig {
                data_dir: dir.path().to_path_buf(),
                altcha_secret: "test".into(),
                pinned_settings: BTreeMap::new(),
                pro: haloid_service::app::ProConfig::default(),
                altcha: None,
                updates: None,
            },
        )
        .await
        .unwrap(),
    );
    let get = |path: String| {
        let app = app.clone();
        async move {
            let response = router(app)
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let location = response
                .headers()
                .get(header::LOCATION)
                .map(|l| l.to_str().unwrap().to_string());
            (response.status(), location)
        }
    };
    let ids = haloid_import::legacy_id::LegacyIds::new("salty", 0);
    let sunset: String =
        sqlx::query_scalar("SELECT id_encoded FROM ih_files WHERE name = 'sunset-Ab12'")
            .fetch_one(&scratch.pool)
            .await
            .unwrap();
    let trips: String = sqlx::query_scalar("SELECT id_encoded FROM ih_albums WHERE name = 'Trips'")
        .fetch_one(&scratch.pool)
        .await
        .unwrap();

    let (status, to) = get(format!("/image/sunset-over-the-bay.{}", ids.encode(1))).await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(to, Some(format!("/f/{sunset}")));
    assert_eq!(
        get(format!("/image/{}", ids.encode(1))).await.1,
        Some(format!("/f/{sunset}"))
    );
    assert_eq!(
        get(format!("/album/trips.{}", ids.encode(1))).await.1,
        Some(format!("/a/{trips}"))
    );
    assert_eq!(
        get("/image/nothing.zzz".into()).await.0,
        StatusCode::NOT_FOUND
    );

    let _ = server
        .execute(format!("DROP DATABASE `{name}`").as_str())
        .await;
    scratch.drop_db().await;
}
