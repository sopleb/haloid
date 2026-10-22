//! The v2 API built so far, end to end over HTTP.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig};
use haloid_service::auth::secrets;
use serde_json::Value;
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
            // Cheap proof-of-work: a debug build solves it in milliseconds.
            altcha: Some(haloid_service::auth::human::AltchaConfig {
                cost: 1,
                counter_min: 1,
                counter_max: 20,
                ..haloid_service::auth::human::AltchaConfig::with_secret("test".into())
            }),
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

fn body(file: &str, extra: &[(&str, &str)]) -> Vec<u8> {
    let bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/media")
            .join(file),
    )
    .unwrap();
    let mut out = Vec::new();
    for (k, v) in extra {
        out.extend_from_slice(
            format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                .as_bytes(),
        );
    }
    out.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(&bytes);
    out.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    out
}

fn upload_request() -> axum::http::request::Builder {
    Request::post("/api/v2/uploads").header(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={BOUNDARY}"),
    )
}

async fn call(app: &Arc<App>, request: Request<Body>) -> (StatusCode, String, Value) {
    let response = router(app.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        content_type,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn session_cookie(app: &Arc<App>, db: &haloid_db::Db) -> String {
    let user = haloid_db::users::create(
        db,
        &haloid_db::users::NewUser {
            username: "browser",
            email: None,
            password_hash: None,
            role: Role::User,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap();
    let token = secrets::new_session_token();
    haloid_db::sessions::create(
        db,
        &haloid_db::sessions::NewSession {
            user_id: user.id,
            token_hash: &secrets::sha256_hex(&token),
            ip: None,
            user_agent: None,
            expires_at: Utc::now() + Duration::days(1),
        },
    )
    .await
    .unwrap();
    let _ = app;
    format!("__Host-haloid_session={token}")
}

#[tokio::test]
async fn a_guest_uploads_and_reads_back_with_real_types() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;

    let (status, _, created) = call(
        &app,
        upload_request()
            .body(Body::from(body(
                "still.jpg",
                &[
                    ("title", "A cat"),
                    ("tags", "cats, pets"),
                    ("nsfw", "false"),
                ],
            )))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let file = &created["file"];
    assert_eq!(file["kind"], "image");
    assert_eq!(file["width"], 32, "real integers in v2");
    assert_eq!(file["nsfw"], false);
    assert_eq!(file["chain"], serde_json::json!(["image", "thumb"]));
    assert_eq!(file["tags"], serde_json::json!(["cats", "pets"]));
    assert_eq!(file["title"], "A cat");
    assert!(
        file["created_at"].as_str().unwrap().contains('T'),
        "RFC 3339"
    );
    assert!(
        file["urls"]["delete"].as_str().is_some(),
        "the creator gets the delete link"
    );
    assert_eq!(created["duplicate"], false);

    let id = file["id"].as_str().unwrap();
    let (status, _, read) = call(
        &app,
        Request::get(format!("/api/v2/files/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["id"], id);
    assert!(
        read["urls"].get("delete").is_none(),
        "a reader never sees the delete link"
    );

    let (status, _, again) = call(
        &app,
        upload_request()
            .body(Body::from(body("still.jpg", &[])))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a duplicate is not a creation");
    assert_eq!(again["duplicate"], true);

    scratch.drop_db().await;
}

#[tokio::test]
async fn errors_are_problem_details_with_real_statuses() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[("uploads.max_size_guest", "500")]).await;

    let (status, content_type, problem) = call(
        &app,
        upload_request()
            .body(Body::from(body("tone.wav", &[])))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(problem["status"], 413);
    assert_eq!(problem["code"], "too_large");

    let (status, _, problem) = call(
        &app,
        upload_request()
            .body(Body::from(body("still.png", &[("nsfw", "maybe")])))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["errors"]["nsfw"][0], "must be true or false");

    let (status, _, problem) = call(
        &app,
        Request::get("/api/v2/files/doesnotexist")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["code"], "not_found");

    let (status, _, problem) = call(
        &app,
        upload_request()
            .header(header::AUTHORIZATION, "Bearer hld_notarealkey_x")
            .body(Body::from(body("still.png", &[])))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["code"], "invalid_api_key");

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_cookie_session_must_prove_the_request_is_ours() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;
    let cookie = session_cookie(&app, &scratch.pool).await;

    let (status, _, _) = call(
        &app,
        upload_request()
            .header(header::COOKIE, &cookie)
            .body(Body::from(body("still.png", &[])))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a cross-site form could send exactly this"
    );

    let (status, _, created) = call(
        &app,
        upload_request()
            .header(header::COOKIE, &cookie)
            .header("x-requested-with", "XMLHttpRequest")
            .header(header::ORIGIN, "http://localhost:8080")
            .body(Body::from(body("still.png", &[])))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    scratch.drop_db().await;
}

#[tokio::test]
async fn an_unapproved_file_is_hidden_from_everyone_but_its_owner() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[("moderation.mode", "all")]).await;
    let user = haloid_db::users::create(
        &scratch.pool,
        &haloid_db::users::NewUser {
            username: "owner",
            email: None,
            password_hash: None,
            role: Role::User,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap();
    let (_, key) = app.auth.create_api_key(Some(user.id), "t").await.unwrap();

    let (status, _, created) = call(
        &app,
        upload_request()
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .body(Body::from(body("still.jpg", &[])))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["file"]["approved"], false);
    let id = created["file"]["id"].as_str().unwrap();

    let (status, _, _) = call(
        &app,
        Request::get(format!("/api/v2/files/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a guest learns nothing");
    let (status, _, _) = call(
        &app,
        Request::get(format!("/api/v2/files/{id}"))
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "its owner sees it");

    scratch.drop_db().await;
}

/// What the browser widget does: fetch a challenge, solve it, base64 it.
async fn solved(app: &Arc<App>) -> String {
    use base64::Engine;
    let (status, _, challenge) = call(
        app,
        Request::get("/api/v2/auth/challenge")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let challenge: altcha::Challenge = serde_json::from_value(challenge).unwrap();
    let solution = altcha::solve_challenge(altcha::SolveChallengeOptions::new(&challenge))
        .unwrap()
        .unwrap();
    base64::engine::general_purpose::STANDARD.encode(
        serde_json::to_vec(&altcha::Payload {
            challenge,
            solution,
        })
        .unwrap(),
    )
}

/// A JSON POST from our own page.
fn ours(path: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-requested-with", "XMLHttpRequest")
        .header(header::ORIGIN, "http://localhost:8080")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn call_with_cookie(
    app: &Arc<App>,
    request: Request<Body>,
) -> (StatusCode, Option<String>, Value) {
    let response = router(app.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn register_log_in_ask_who_i_am_and_log_out() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;
    // Someone registered first, and administers the instance; alice is a
    // member.
    haloid_db::users::create(
        &scratch.pool,
        &haloid_db::users::NewUser {
            username: "founder",
            email: None,
            password_hash: None,
            role: Role::Admin,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap();

    let (status, _, me) = call(
        &app,
        ours(
            "/api/v2/auth/register",
            serde_json::json!({"username": "alice", "password": "correct horse battery", "altcha": solved(&app).await}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{me}");
    assert_eq!(me["username"], "alice");
    assert_eq!(me["role"], "user");

    let (status, cookie, me) = call_with_cookie(
        &app,
        ours(
            "/api/v2/auth/login",
            serde_json::json!({"login": "alice", "password": "correct horse battery", "altcha": solved(&app).await}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    let cookie = cookie.expect("a session cookie");
    assert!(cookie.starts_with("__Host-haloid_session="), "{cookie}");
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/", "Secure"] {
        assert!(cookie.contains(attribute), "{cookie} lacks {attribute}");
    }
    let pair = cookie.split(';').next().unwrap().to_string();

    let (status, _, me) = call(
        &app,
        Request::get("/api/v2/auth/me")
            .header(header::COOKIE, &pair)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["username"], "alice");

    let (status, _, config) = call(
        &app,
        Request::get("/api/v2/config")
            .header(header::COOKIE, &pair)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config["signed_in"], true);
    assert_eq!(
        config["uploads"]["max_size"],
        64 * 1024 * 1024,
        "a member's limit"
    );

    let mut logout = ours("/api/v2/auth/logout", Value::Null);
    logout
        .headers_mut()
        .insert(header::COOKIE, pair.parse().unwrap());
    let (status, cleared, _) = call_with_cookie(&app, logout).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(cleared.unwrap().contains("Max-Age=0"));

    let (status, _, problem) = call(
        &app,
        Request::get("/api/v2/auth/me")
            .header(header::COOKIE, &pair)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the old cookie is dead");
    assert_eq!(problem["code"], "unauthenticated");

    scratch.drop_db().await;
}

#[tokio::test]
async fn sign_in_forms_must_come_from_our_own_page() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;
    let request = Request::post("/api/v2/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::from(
            serde_json::json!({"login": "a", "password": "b", "altcha": "c"}).to_string(),
        ))
        .unwrap();
    let (status, _, _) = call(&app, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, problem) = call(
        &app,
        ours("/api/v2/auth/login", serde_json::json!({"login": 5})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "malformed bodies are problems too"
    );
    assert_eq!(problem["code"], "invalid_params");

    let (status, _, problem) = call(
        &app,
        ours(
            "/api/v2/auth/login",
            serde_json::json!({"login": "a", "password": "b", "altcha": "not a solution"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["code"], "human_verification_failed");

    scratch.drop_db().await;
}

#[tokio::test]
async fn explore_profiles_and_albums_follow_the_visibility_rules() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;
    let owner = haloid_db::users::create(
        &scratch.pool,
        &haloid_db::users::NewUser {
            username: "carol",
            email: None,
            password_hash: None,
            role: Role::User,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap();
    let (_, key) = app.auth.create_api_key(Some(owner.id), "t").await.unwrap();
    let bearer = format!("Bearer {key}");
    let hidden = haloid_db::albums::create(
        &scratch.pool,
        &haloid_db::albums::NewAlbum {
            user_id: Some(owner.id),
            parent_id: None,
            name: "Secret",
            description: None,
            privacy: haloid_core::files::AlbumPrivacy::Private,
            password_hash: None,
        },
    )
    .await
    .unwrap();

    let mut public_ids = Vec::new();
    for name in ["still.jpg", "still.png", "still.gif"] {
        let (status, _, created) = call(
            &app,
            upload_request()
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::from(body(name, &[])))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        public_ids.push(created["file"]["id"].as_str().unwrap().to_string());
    }
    let (_, _, secret) = call(
        &app,
        upload_request()
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::from(body(
                "still.webp",
                &[("album", hidden.id_encoded.as_str())],
            )))
            .unwrap(),
    )
    .await;
    let secret_id = secret["file"]["id"].as_str().unwrap().to_string();

    let ids = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_str().unwrap().to_string())
            .collect()
    };
    let guest_get = |path: String| Request::get(path).body(Body::empty()).unwrap();

    // Explore, two at a time, through the cursor.
    let (status, _, first) = call(&app, guest_get("/api/v2/files?limit=2".into())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&first).len(), 2);
    let cursor = first["next"].as_str().expect("a next page").to_string();
    let (_, _, second) = call(
        &app,
        guest_get(format!("/api/v2/files?limit=2&cursor={cursor}")),
    )
    .await;
    let mut all = ids(&first);
    all.extend(ids(&second));
    assert_eq!(second["next"], Value::Null);
    public_ids.reverse();
    assert_eq!(
        all, public_ids,
        "newest first, the private-album file nowhere"
    );

    let (_, _, profile) = call(&app, guest_get("/api/v2/users/carol".into())).await;
    assert_eq!(profile["file_count"], 4);
    let (_, _, guest_view) = call(&app, guest_get("/api/v2/users/carol/files".into())).await;
    assert!(!ids(&guest_view).contains(&secret_id));
    let (_, _, owner_view) = call(
        &app,
        Request::get("/api/v2/users/carol/files")
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        ids(&owner_view).contains(&secret_id),
        "the owner sees everything of theirs"
    );

    let album_path = format!("/api/v2/albums/{}", hidden.id_encoded);
    let (status, _, _) = call(&app, guest_get(album_path.clone())).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a private album is not there for a guest"
    );
    let (status, _, album) = call(
        &app,
        Request::get(&album_path)
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(album["privacy"], "private");
    let (_, _, albums) = call(&app, guest_get("/api/v2/users/carol/albums".into())).await;
    assert!(
        albums["items"].as_array().unwrap().is_empty(),
        "nor is it listed"
    );

    let (status, _, problem) = call(&app, guest_get("/api/v2/files?sort=sideways".into())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(problem["errors"]["sort"].is_array());

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_secure_site_ignores_an_unprefixed_session_cookie() {
    // A sibling subdomain can set `haloid_session` but never
    // `__Host-haloid_session`; honouring the former would let it fix a
    // visitor's session. The test site is localhost, which counts as secure.
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir, &[]).await;
    let hardened = session_cookie(&app, &scratch.pool).await;
    let planted = hardened.replacen("__Host-haloid_session", "haloid_session", 1);

    let me = |cookie: String| {
        Request::get("/api/v2/auth/me")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap()
    };
    let (status, _, _) = call(&app, me(hardened)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&app, me(planted)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the planted cookie was honoured"
    );

    scratch.drop_db().await;
}
