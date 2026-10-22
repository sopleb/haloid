//! Storages added or changed in the admin area are used at once.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig, ProConfig};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const BOUNDARY: &str = "haloidliveboundary";

async fn app(db: &haloid_db::Db, dir: &TempDir, licence_key: Option<String>) -> Arc<App> {
    Arc::new(
        App::start_default(
            db.clone(),
            AppConfig {
                data_dir: dir.path().to_path_buf(),
                altcha_secret: "test".into(),
                pinned_settings: BTreeMap::new(),
                pro: ProConfig {
                    licence_key,
                    ..ProConfig::default()
                },
                altcha: None,
                updates: None,
            },
        )
        .await
        .unwrap(),
    )
}

async fn bearer(app: &App, db: &haloid_db::Db, name: &str, role: Role) -> String {
    let user = haloid_db::users::create(
        db,
        &haloid_db::users::NewUser {
            username: name,
            email: Some(&format!("{name}@example.com")),
            password_hash: None,
            role,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap();
    let (_, key) = app.auth.create_api_key(Some(user.id), "t").await.unwrap();
    format!("Bearer {key}")
}

async fn call(
    app: &Arc<App>,
    method: Method,
    path: &str,
    auth: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    let body = match body {
        Some(v) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let response = router(app.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn upload(app: &Arc<App>, auth: &str, file: &str) -> (StatusCode, Value) {
    let bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/media")
            .join(file),
    )
    .unwrap();
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let response = router(app.clone())
        .oneshot(
            Request::post("/api/v2/uploads")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .header(header::AUTHORIZATION, auth)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn fetch(app: &Arc<App>, path: &str) -> (StatusCode, usize) {
    let response = router(app.clone())
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.len())
}

/// The path part of a file URL.
fn path_of(url: &Value) -> String {
    let url = url.as_str().unwrap();
    let after = url.split_once("://").unwrap().1;
    after[after.find('/').unwrap()..].to_string()
}

#[tokio::test]
async fn a_storage_added_or_changed_in_the_admin_area_is_used_without_a_restart() {
    let scratch = migrated_or_skip!();
    let db = &scratch.pool;
    let dir = TempDir::new().unwrap();
    let app = app(db, &dir, None).await;
    let admin = bearer(&app, db, "boss", Role::Admin).await;

    // A storage that cannot work is refused, and not saved.
    let (status, _) = call(&app, Method::POST, "/api/v2/storages", Some(&admin), Some(json!({
        "name": "typo", "kind": "local", "config": {"base_pth": "/srv"}, "url_prefix": "http://localhost:8080/typo"
    }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, list) = call(&app, Method::GET, "/api/v2/storages", Some(&admin), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1, "only main");

    // Added with the highest priority: the next upload goes there, and is
    // served from its prefix straight away.
    let (status, created) = call(&app, Method::POST, "/api/v2/storages", Some(&admin), Some(json!({
        "name": "archive", "kind": "local", "config": {}, "url_prefix": "http://localhost:8080/archive", "priority": 10
    }))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (status, uploaded) = upload(&app, &admin, "still.jpg").await;
    assert_eq!(status, StatusCode::CREATED, "{uploaded}");
    let image = path_of(&uploaded["file"]["urls"]["image"]);
    assert!(image.starts_with("/archive/"), "{image}");
    let (status, len) = fetch(&app, &image).await;
    assert_eq!(status, StatusCode::OK);
    assert!(len > 0);

    // Moved to another prefix: served there at once, and no longer at the old.
    let id = created["id"].as_u64().unwrap();
    let (status, _) = call(&app, Method::PATCH, &format!("/api/v2/storages/{id}"), Some(&admin), Some(json!({
        "name": "archive", "kind": "local", "config": {}, "url_prefix": "http://localhost:8080/vault", "priority": 10
    }))).await;
    assert_eq!(status, StatusCode::OK);
    let moved = image.replacen("/archive/", "/vault/", 1);
    assert_eq!(fetch(&app, &moved).await.0, StatusCode::OK);
    assert_ne!(fetch(&app, &image).await.0, StatusCode::OK);

    scratch.drop_db().await;
}
