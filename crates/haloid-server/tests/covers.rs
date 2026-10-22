//! Album covers and file views over HTTP.

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

const BOUNDARY: &str = "haloidcoverboundary";

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

async fn upload(app: &Arc<App>, auth: &str, file: &str, album: &str) -> (StatusCode, Value) {
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
    body.extend_from_slice(
        format!(
            "\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"album\"\r\n\r\n{album}"
        )
        .as_bytes(),
    );
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

#[tokio::test]
async fn an_album_card_shows_its_cover_and_a_file_page_counts_its_viewers() {
    let scratch = migrated_or_skip!();
    let db = &scratch.pool;
    let dir = TempDir::new().unwrap();
    let app = app(db, &dir, None).await;
    let owner = bearer(&app, db, "mira", Role::User).await;
    let other = bearer(&app, db, "tomas", Role::User).await;

    let (status, album) = call(
        &app,
        Method::POST,
        "/api/v2/albums",
        Some(&owner),
        Some(json!({"name": "Coast", "privacy": "public"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{album}");
    let album_id = album["id"].as_str().unwrap().to_string();
    assert_eq!(album["cover"], Value::Null, "nothing in it yet");

    let (_, first) = upload(&app, &owner, "still.jpg", &album_id).await;
    let (_, second) = upload(&app, &owner, "still.png", &album_id).await;
    let thumb = |file: &Value| file["file"]["urls"]["thumb"].clone();

    // The first file in the album's order, until one is chosen.
    let (_, list) = call(&app, Method::GET, "/api/v2/users/mira/albums", None, None).await;
    assert_eq!(list["items"][0]["cover"], thumb(&first));
    let path = format!("/api/v2/albums/{album_id}");
    let (status, chosen) = call(
        &app,
        Method::PATCH,
        &path,
        Some(&owner),
        Some(json!({"cover": second["file"]["id"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{chosen}");
    assert_eq!(chosen["cover"], thumb(&second));
    let (_, list) = call(&app, Method::GET, "/api/v2/users/mira/albums", None, None).await;
    assert_eq!(list["items"][0]["cover"], thumb(&second));

    // Only one of its own files; null goes back to the first.
    let (_, loose) = upload(&app, &owner, "still.webp", "").await;
    let (status, _) = call(
        &app,
        Method::PATCH,
        &path,
        Some(&owner),
        Some(json!({"cover": loose["file"]["id"]})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, reset) = call(
        &app,
        Method::PATCH,
        &path,
        Some(&owner),
        Some(json!({"cover": null})),
    )
    .await;
    assert_eq!(reset["cover"], thumb(&first));

    // Views: each viewer once per half hour, and never the owner.
    let id = first["file"]["id"].as_str().unwrap().to_string();
    let view = format!("/api/v2/files/{id}/view");
    for auth in [
        Some(other.as_str()),
        Some(other.as_str()),
        None,
        Some(owner.as_str()),
    ] {
        let (status, _) = call(&app, Method::POST, &view, auth, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (_, file) = call(
        &app,
        Method::GET,
        &format!("/api/v2/files/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        file["views"], 2,
        "tomas once, the guest once, the owner not at all"
    );
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v2/files/nosuchfile/view",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    scratch.drop_db().await;
}
