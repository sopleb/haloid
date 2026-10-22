//! The v2 write surface over HTTP: editing and deleting files, bulk
//! actions, and albums.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{Duration, Utc};
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig};
use haloid_service::auth::secrets;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const BOUNDARY: &str = "haloidv2boundary";

async fn app(db: &haloid_db::Db, dir: &TempDir, settings: &[(&str, &str)]) -> Arc<App> {
    let app = App::start_default(
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
    if !settings.is_empty() {
        app.settings
            .update(
                &settings
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
            .await
            .unwrap();
    }
    Arc::new(app)
}

async fn user(db: &haloid_db::Db, name: &str) -> haloid_core::account::User {
    haloid_db::users::create(
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
    .unwrap()
}

async fn bearer(app: &Arc<App>, db: &haloid_db::Db, name: &str) -> String {
    let user = user(db, name).await;
    let (_, key) = app.auth.create_api_key(Some(user.id), "t").await.unwrap();
    format!("Bearer {key}")
}

async fn call(app: &Arc<App>, request: Request<Body>) -> (StatusCode, Value) {
    let response = router(app.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn json_request(method: Method, path: &str, auth: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn bare(method: Method, path: &str, auth: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    builder.body(Body::empty()).unwrap()
}

/// Upload a fixture, returning the response's `file` object.
async fn upload(app: &Arc<App>, auth: Option<&str>, file: &str) -> Value {
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
    let mut builder = Request::post("/api/v2/uploads").header(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={BOUNDARY}"),
    );
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    let (status, created) = call(app, builder.body(Body::from(body)).unwrap()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created["file"].clone()
}

fn id(file: &Value) -> String {
    file["id"].as_str().unwrap().to_string()
}

/// The path part of one of our own absolute URLs.
fn path_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    after_scheme
        .find('/')
        .map_or("/".to_string(), |i| after_scheme[i..].to_string())
}

async fn create_album(app: &Arc<App>, auth: &str, body: Value) -> Value {
    let (status, album) = call(
        app,
        json_request(Method::POST, "/api/v2/albums", Some(auth), body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{album}");
    album
}

#[tokio::test]
async fn an_owner_edits_a_file_and_nobody_else_can() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[("uploads.stop_words", "forbidden")]).await;
    let alice = bearer(&app, &scratch.pool, "alice").await;
    let mallory = bearer(&app, &scratch.pool, "mallory").await;
    let file = id(&upload(&app, Some(&alice), "still.png").await);
    let path = format!("/api/v2/files/{file}");
    let album = create_album(&app, &alice, json!({"name": "Holiday"})).await;
    let theirs = create_album(&app, &mallory, json!({"name": "Mine"})).await;

    let (status, edited) = call(
        &app,
        json_request(
            Method::PATCH,
            &path,
            Some(&alice),
            json!({
                "title": "  A beach  ",
                "description": "Sand.",
                "nsfw": true,
                "tags": ["sea", "sun", " "],
                "album": album["id"],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["title"], "A beach", "trimmed");
    assert_eq!(edited["description"], "Sand.");
    assert_eq!(edited["nsfw"], true);
    assert_eq!(edited["tags"], json!(["sea", "sun"]));
    assert_eq!(edited["album"], album["id"]);

    // Leaving a field out keeps it; null clears it.
    let (status, edited) = call(
        &app,
        json_request(
            Method::PATCH,
            &path,
            Some(&alice),
            json!({"title": null, "album": null}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["title"], Value::Null);
    assert_eq!(edited["album"], Value::Null);
    assert_eq!(edited["description"], "Sand.", "untouched");
    assert_eq!(edited["tags"], json!(["sea", "sun"]), "untouched");

    for (body, code) in [
        (json!({"title": "a forbidden word"}), "forbidden_content"),
        (json!({"title": "x".repeat(256)}), "invalid_params"),
        (json!({"album": theirs["id"]}), "invalid_params"),
        (json!({"colour": "red"}), "invalid_params"),
    ] {
        let (status, problem) = call(
            &app,
            json_request(Method::PATCH, &path, Some(&alice), body.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {problem}"
        );
        assert_eq!(problem["code"], code, "{body}: {problem}");
    }

    // Someone else's file, or no one's session: not found, not forbidden.
    for auth in [Some(mallory.as_str()), None] {
        let (status, problem) = call(
            &app,
            json_request(Method::PATCH, &path, auth, json!({"title": "mine now"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
    }
    let (_, unchanged) = call(&app, bare(Method::GET, &path, Some(&alice))).await;
    assert_eq!(unchanged["title"], Value::Null);

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_delete_link_works_once_for_whoever_holds_it() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;
    let uploaded = upload(&app, None, "still.png").await;
    let file = id(&uploaded);
    let delete_link = uploaded["urls"]["delete"].as_str().unwrap();
    let token = delete_link.rsplit('/').next().unwrap();
    let image = path_of(uploaded["urls"]["image"].as_str().unwrap());
    let path = format!("/api/v2/files/{file}");

    let (status, _) = call(&app, bare(Method::GET, &image, None)).await;
    assert_eq!(status, StatusCode::OK, "served before deletion");

    let with_token = |token: &str| {
        Request::delete(&path)
            .header("x-delete-token", token)
            .body(Body::empty())
            .unwrap()
    };
    let (status, _) = call(&app, with_token("not-the-token")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, bare(Method::DELETE, &path, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a guest without the token");

    let (status, _) = call(&app, with_token(token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, bare(Method::GET, &path, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, bare(Method::GET, &image, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the bytes are quarantined");

    // Retained for audit, not destroyed.
    let row = haloid_db::files::find_by_id_encoded(&scratch.pool, &file)
        .await
        .unwrap()
        .expect("the row is kept");
    assert!(row.deletion.is_some());

    scratch.drop_db().await;
}

#[tokio::test]
async fn albums_are_created_arranged_and_deleted_both_ways() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;
    let alice = bearer(&app, &scratch.pool, "alice").await;
    let mallory = bearer(&app, &scratch.pool, "mallory").await;

    let (status, problem) = call(
        &app,
        json_request(Method::POST, "/api/v2/albums", None, json!({"name": "x"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "guests have no albums: {problem}"
    );
    let (status, problem) = call(
        &app,
        json_request(
            Method::POST,
            "/api/v2/albums",
            Some(&alice),
            json!({"name": "Locked", "privacy": "password"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");

    let trips = create_album(
        &app,
        &alice,
        json!({"name": "Trips", "privacy": "unlisted"}),
    )
    .await;
    assert_eq!(trips["privacy"], "unlisted");
    let rome = create_album(
        &app,
        &alice,
        json!({"name": "Rome", "parent": trips["id"], "description": "2024"}),
    )
    .await;
    let (status, problem) = call(
        &app,
        json_request(
            Method::POST,
            "/api/v2/albums",
            Some(&mallory),
            json!({"name": "Squat", "parent": trips["id"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");

    // Edit: rename, lock with a password, and refuse a move into itself.
    let rome_path = format!("/api/v2/albums/{}", rome["id"].as_str().unwrap());
    let trips_path = format!("/api/v2/albums/{}", trips["id"].as_str().unwrap());
    let (status, edited) = call(
        &app,
        json_request(
            Method::PATCH,
            &rome_path,
            Some(&alice),
            json!({"name": "Roma", "privacy": "password", "password": "lupa", "description": null}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["name"], "Roma");
    assert_eq!(edited["privacy"], "password");
    assert_eq!(edited["description"], Value::Null);
    let (status, problem) = call(
        &app,
        json_request(
            Method::PATCH,
            &trips_path,
            Some(&alice),
            json!({"parent": rome["id"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    let (status, _) = call(
        &app,
        json_request(
            Method::PATCH,
            &trips_path,
            Some(&mallory),
            json!({"name": "Mine"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Three files into Trips, then a new order.
    let mut in_trips = Vec::new();
    for name in ["still.png", "still.jpg", "still.gif"] {
        let file = id(&upload(&app, Some(&alice), name).await);
        let (status, _) = call(
            &app,
            json_request(
                Method::PATCH,
                &format!("/api/v2/files/{file}"),
                Some(&alice),
                json!({"album": trips["id"]}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        in_trips.push(file);
    }
    let order = json!({"files": [in_trips[2], in_trips[0]]});
    let (status, _) = call(
        &app,
        json_request(
            Method::PUT,
            &format!("{trips_path}/order"),
            Some(&alice),
            order,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, page) = call(
        &app,
        bare(Method::GET, &format!("{trips_path}/files"), Some(&alice)),
    )
    .await;
    let listed: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, vec![&in_trips[2], &in_trips[0], &in_trips[1]]);

    // A file in Roma, deleted with its album; Trips' files are kept when
    // Trips goes with `keep`.
    let in_rome = id(&upload(&app, Some(&alice), "still.webp").await);
    call(
        &app,
        json_request(
            Method::PATCH,
            &format!("/api/v2/files/{in_rome}"),
            Some(&alice),
            json!({"album": rome["id"]}),
        ),
    )
    .await;
    let (status, problem) = call(&app, bare(Method::DELETE, &rome_path, Some(&alice))).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "contents is required: {problem}"
    );
    let (status, _) = call(
        &app,
        bare(
            Method::DELETE,
            &format!("{rome_path}?contents=delete"),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        bare(
            Method::GET,
            &format!("/api/v2/files/{in_rome}"),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "deleted with its album");

    let (status, _) = call(
        &app,
        bare(
            Method::DELETE,
            &format!("{trips_path}?contents=keep"),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for file in &in_trips {
        let (status, kept) = call(
            &app,
            bare(Method::GET, &format!("/api/v2/files/{file}"), Some(&alice)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "kept: {kept}");
        assert_eq!(kept["album"], Value::Null);
    }
    let (status, _) = call(&app, bare(Method::GET, &trips_path, Some(&alice))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    scratch.drop_db().await;
}

#[tokio::test]
async fn bulk_actions_report_each_file() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;
    let alice = bearer(&app, &scratch.pool, "alice").await;
    let mallory = bearer(&app, &scratch.pool, "mallory").await;
    let mine = [
        id(&upload(&app, Some(&alice), "still.png").await),
        id(&upload(&app, Some(&alice), "still.jpg").await),
    ];
    let theirs = id(&upload(&app, Some(&mallory), "still.gif").await);
    let album = create_album(&app, &alice, json!({"name": "Box"})).await;

    let (status, result) = call(
        &app,
        json_request(
            Method::POST,
            "/api/v2/files/bulk",
            Some(&alice),
            json!({"action": "move", "ids": [mine[0], theirs, mine[1]], "album": album["id"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["done"], json!([mine[0], mine[1]]));
    assert_eq!(
        result["failed"],
        json!([{"id": theirs, "code": "not_found"}])
    );
    let (_, moved) = call(
        &app,
        bare(
            Method::GET,
            &format!("/api/v2/files/{}", mine[1]),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(moved["album"], album["id"]);

    let (status, result) = call(
        &app,
        json_request(
            Method::POST,
            "/api/v2/files/bulk",
            Some(&alice),
            json!({"action": "delete", "ids": mine}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["done"], json!(mine));
    let (status, _) = call(
        &app,
        bare(
            Method::GET,
            &format!("/api/v2/files/{}", mine[0]),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let too_many: Vec<String> = (0..101).map(|i| format!("id{i}")).collect();
    let (status, problem) = call(
        &app,
        json_request(
            Method::POST,
            "/api/v2/files/bulk",
            Some(&alice),
            json!({"action": "delete", "ids": too_many}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_cookie_write_must_come_from_our_page() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;
    let owner = user(&scratch.pool, "browser").await;
    let token = secrets::new_session_token();
    haloid_db::sessions::create(
        &scratch.pool,
        &haloid_db::sessions::NewSession {
            user_id: owner.id,
            token_hash: &secrets::sha256_hex(&token),
            ip: None,
            user_agent: None,
            expires_at: Utc::now() + Duration::days(1),
        },
    )
    .await
    .unwrap();
    let cookie = format!("__Host-haloid_session={token}");
    let request = |ours: bool| {
        let mut builder = Request::post("/api/v2/albums")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie);
        if ours {
            builder = builder
                .header("x-requested-with", "XMLHttpRequest")
                .header(header::ORIGIN, "http://localhost:8080");
        }
        builder
            .body(Body::from(json!({"name": "Mine"}).to_string()))
            .unwrap()
    };
    let (status, problem) = call(&app, request(false)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    let (status, album) = call(&app, request(true)).await;
    assert_eq!(status, StatusCode::CREATED, "{album}");

    scratch.drop_db().await;
}
