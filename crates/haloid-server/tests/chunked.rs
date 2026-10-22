//! Chunked uploads over HTTP, with chunks small enough that the fixtures
//! take many.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::Utc;
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig};
use haloid_service::chunked::ChunkedUploads;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const CHUNK: u32 = 100;

async fn app(db: &haloid_db::Db, dir: &TempDir) -> Arc<App> {
    let mut app = App::start_default(
        db.clone(),
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
    .unwrap();
    app.chunked = ChunkedUploads::new(app.db.clone(), app.settings.clone(), app.uploads.clone())
        .with_chunk_size(CHUNK);
    Arc::new(app)
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/media")
            .join(name),
    )
    .unwrap()
}

async fn call(
    app: &Arc<App>,
    method: Method,
    path: &str,
    auth: Option<&str>,
    body: Body,
    json_body: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if json_body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
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

async fn open(app: &Arc<App>, auth: Option<&str>, body: Value) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/api/v2/uploads/sessions",
        auth,
        Body::from(body.to_string()),
        true,
    )
    .await
}

async fn send(
    app: &Arc<App>,
    auth: Option<&str>,
    id: &str,
    n: u32,
    bytes: &[u8],
) -> (StatusCode, Value) {
    call(
        app,
        Method::PUT,
        &format!("/api/v2/uploads/sessions/{id}/chunks/{n}"),
        auth,
        Body::from(bytes.to_vec()),
        false,
    )
    .await
}

async fn bearer(app: &App, db: &haloid_db::Db, name: &str) -> String {
    let user = haloid_db::users::create(
        db,
        &haloid_db::users::NewUser {
            username: name,
            email: None,
            password_hash: None,
            role: Role::User,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap();
    let (_, key) = app.auth.create_api_key(Some(user.id), "t").await.unwrap();
    format!("Bearer {key}")
}

fn chunks(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.chunks(CHUNK as usize).collect()
}

#[tokio::test]
async fn a_file_arrives_in_any_order_and_completes_once() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let bytes = fixture("still.png");
    let parts = chunks(&bytes);
    assert!(parts.len() > 2, "the fixture must take several chunks");

    let (status, session) = open(
        &app,
        None,
        json!({"filename": "still.png", "size": bytes.len(), "title": "Pieces", "tags": ["a"]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let id = session["id"].as_str().unwrap().to_string();
    assert_eq!(session["chunk_size"], CHUNK);
    assert_eq!(session["chunks_total"], parts.len());
    assert_eq!(session["received"], json!([]));

    // Backwards, with the second chunk twice.
    for n in (0..parts.len()).rev() {
        let (status, state) = send(&app, None, &id, n as u32, parts[n]).await;
        assert_eq!(status, StatusCode::OK, "chunk {n}: {state}");
        if n == 1 {
            break;
        }
    }
    let (status, _) = send(&app, None, &id, 1, parts[1]).await;
    assert_eq!(status, StatusCode::OK, "sending again is fine");

    // The wrong length, and a chunk that does not exist.
    let (status, problem) = send(&app, None, &id, 1, &parts[1][..10]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    let (status, _) = send(&app, None, &id, parts.len() as u32, b"x").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Ask before resuming: chunk 0 is the one missing.
    let (status, state) = call(
        &app,
        Method::GET,
        &format!("/api/v2/uploads/sessions/{id}"),
        None,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["missing"], json!([0]));
    let complete = format!("/api/v2/uploads/sessions/{id}/complete");
    let (status, problem) = call(&app, Method::POST, &complete, None, Body::empty(), false).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "not yet: {problem}"
    );

    send(&app, None, &id, 0, parts[0]).await;
    let (status, created) = call(&app, Method::POST, &complete, None, Body::empty(), false).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["file"]["title"], "Pieces");
    assert_eq!(created["file"]["tags"], json!(["a"]));
    assert_eq!(created["file"]["width"], 32);
    assert!(
        created["file"]["urls"]["delete"].is_string(),
        "a guest gets the delete link"
    );

    let (status, _) = call(&app, Method::POST, &complete, None, Body::empty(), false).await;
    assert_eq!(status, StatusCode::CONFLICT, "only once");
    let (status, _) = send(&app, None, &id, 0, parts[0]).await;
    assert_eq!(status, StatusCode::CONFLICT, "no chunks after completion");
    assert!(
        !dir.path().join("tmp/sessions").join(&id).exists(),
        "the chunks are cleaned up"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn sessions_belong_to_their_uploader_and_respect_limits() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let alice = bearer(&app, &scratch.pool, "alice").await;
    let mallory = bearer(&app, &scratch.pool, "mallory").await;

    let (status, problem) = open(
        &app,
        None,
        json!({"filename": "big.mp4", "size": 1u64 << 40}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "refused before a byte: {problem}"
    );
    let (status, _) = open(&app, None, json!({"filename": "x.png", "size": 0})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, session) = open(
        &app,
        Some(&alice),
        json!({"filename": "a.png", "size": 250}),
    )
    .await;
    let id = session["id"].as_str().unwrap();
    let path = format!("/api/v2/uploads/sessions/{id}");
    for auth in [Some(mallory.as_str()), None] {
        let (status, _) = call(&app, Method::GET, &path, auth, Body::empty(), false).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = send(&app, auth, id, 0, &[0u8; 100]).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // Abandoning drops it and its chunks.
    send(&app, Some(&alice), id, 0, &[0u8; 100]).await;
    let (status, _) = call(
        &app,
        Method::DELETE,
        &path,
        Some(&alice),
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, Method::GET, &path, Some(&alice), Body::empty(), false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!dir.path().join("tmp/sessions").join(id).exists());

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_refused_file_ends_the_session_and_stale_ones_expire() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;

    // Not an image at all, whatever its name says.
    let junk = vec![b'z'; 150];
    let (_, session) = open(
        &app,
        None,
        json!({"filename": "fake.png", "size": junk.len()}),
    )
    .await;
    let id = session["id"].as_str().unwrap().to_string();
    for (n, part) in chunks(&junk).into_iter().enumerate() {
        send(&app, None, &id, n as u32, part).await;
    }
    let (status, problem) = call(
        &app,
        Method::POST,
        &format!("/api/v2/uploads/sessions/{id}/complete"),
        None,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    let (_, state) = call(
        &app,
        Method::GET,
        &format!("/api/v2/uploads/sessions/{id}"),
        None,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(state["status"], "failed");
    assert!(!dir.path().join("tmp/sessions").join(&id).exists());

    // Left alone past its lifetime: the job drops its chunks.
    let (_, stale) = open(&app, None, json!({"filename": "s.png", "size": 50})).await;
    let stale_id = stale["id"].as_str().unwrap().to_string();
    send(&app, None, &stale_id, 0, &[1u8; 50]).await;
    sqlx::query("UPDATE ih_upload_sessions SET expires_at = ? WHERE id = ?")
        .bind(Utc::now() - chrono::Duration::hours(1))
        .bind(&stale_id)
        .execute(&scratch.pool)
        .await
        .unwrap();
    assert_eq!(
        app.chunked.expire(Utc::now()).await.unwrap(),
        1,
        "only the session past its lifetime"
    );
    assert!(!dir.path().join("tmp/sessions").join(&stale_id).exists());
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/api/v2/uploads/sessions/{stale_id}"),
        None,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    scratch.drop_db().await;
}
