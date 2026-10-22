//! The licence over HTTP: what Community refuses, installing a key, and
//! what it changes.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{Duration, Utc};
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_licence::{Kind, Licence};
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig, ProConfig};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const BOUNDARY: &str = "haloidlicenceboundary";
const SIGNER: [u8; 32] = [42; 32];

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
                    trusted_keys: vec![haloid_licence::public_key(&SIGNER)],
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

fn key(instance: &str, kind: Kind, expires_in: Option<Duration>) -> String {
    haloid_licence::sign(
        &Licence {
            id: "lic_1".into(),
            instance: instance.into(),
            licensee: "Example Ltd".into(),
            kind,
            issued_at: Utc::now(),
            expires_at: expires_in.map(|d| Utc::now() + d),
        },
        &SIGNER,
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

#[tokio::test]
async fn community_refuses_pro_features_until_a_key_for_this_instance_is_installed() {
    let scratch = migrated_or_skip!();
    let db = &scratch.pool;
    let dir = TempDir::new().unwrap();
    let app = app(db, &dir, None).await;
    let admin = bearer(&app, db, "boss", Role::Admin).await;
    let member = bearer(&app, db, "member", Role::User).await;

    // Community: branded, no audio offered or accepted, no external storage.
    let (_, config) = call(&app, Method::GET, "/api/v2/config", None, None).await;
    assert_eq!(config["branding"], true);
    let extensions = config["uploads"]["extensions"].as_array().unwrap();
    assert!(!extensions.contains(&json!("mp3")) && extensions.contains(&json!("mp4")));
    let (status, refused) = upload(&app, &admin, "tone.mp3").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "pro_required");
    let (status, refused) = call(
        &app,
        Method::POST,
        "/api/v2/storages",
        Some(&admin),
        Some(json!({"name": "bucket", "kind": "s3", "config": {"bucket": "b"}, "url_prefix": "https://cdn.example.com"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "pro_required");

    // The admin area shows the instance id a key must name.
    let (status, _) = call(&app, Method::GET, "/api/v2/licence", Some(&member), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, licence) = call(&app, Method::GET, "/api/v2/licence", Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(licence["state"], "none");
    assert_eq!(licence["storage_kinds"], json!(["local"]));
    let instance = licence["instance_id"].as_str().unwrap().to_string();
    assert!(
        instance.starts_with("inst_") && instance.len() == 25,
        "{instance}"
    );

    // A key for another instance, or not signed by us, is refused.
    let (status, refused) = call(
        &app,
        Method::PUT,
        "/api/v2/licence",
        Some(&admin),
        Some(json!({"key": key("inst_elsewhere", Kind::Standard, Some(Duration::days(730)))})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    let forged = haloid_licence::sign(
        &haloid_licence::verify(
            &key(&instance, Kind::Perpetual, None),
            &[haloid_licence::public_key(&SIGNER)],
        )
        .unwrap(),
        &[1; 32],
    );
    let (status, _) = call(
        &app,
        Method::PUT,
        "/api/v2/licence",
        Some(&admin),
        Some(json!({"key": forged})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A trial for this instance: everything Pro, at once.
    let trial = key(&instance, Kind::Trial, Some(Duration::days(90)));
    let (status, licence) = call(
        &app,
        Method::PUT,
        "/api/v2/licence",
        Some(&admin),
        Some(json!({"key": trial})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{licence}");
    assert_eq!(licence["state"], "active");
    assert_eq!(licence["pro"], true);
    assert_eq!(licence["licence"]["kind"], "trial");
    let (_, config) = call(&app, Method::GET, "/api/v2/config", None, None).await;
    assert_eq!(config["branding"], false);
    assert!(
        config["uploads"]["extensions"]
            .as_array()
            .unwrap()
            .contains(&json!("mp3"))
    );
    let (status, uploaded) = upload(&app, &admin, "tone.mp3").await;
    assert_eq!(status, StatusCode::CREATED, "{uploaded}");
    // Pro, but this is the Community build: there is no S3 code to use.
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v2/storages",
        Some(&admin),
        Some(json!({"name": "bucket", "kind": "s3", "config": {"bucket": "b"}, "url_prefix": "https://cdn.example.com"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The key survives a restart; removing it is audited and immediate.
    let restarted = self::app(db, &dir, None).await;
    let (_, licence) = call(
        &restarted,
        Method::GET,
        "/api/v2/licence",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(licence["state"], "active");
    assert_eq!(licence["instance_id"], instance.as_str());
    let (status, _) = call(
        &restarted,
        Method::DELETE,
        "/api/v2/licence",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, config) = call(&restarted, Method::GET, "/api/v2/config", None, None).await;
    assert_eq!(config["branding"], true);
    let (_, log) = call(
        &restarted,
        Method::GET,
        "/api/v2/admin/audit",
        Some(&admin),
        None,
    )
    .await;
    let actions: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["action"].as_str())
        .collect();
    assert!(
        actions.contains(&"licence.install") && actions.contains(&"licence.remove"),
        "{actions:?}"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_key_from_the_environment_wins_and_cannot_be_changed_from_the_admin_area() {
    let scratch = migrated_or_skip!();
    let db = &scratch.pool;
    let dir = TempDir::new().unwrap();
    // The instance id is made at first start; the operator then sets a key
    // for it in the environment.
    let first = app(db, &dir, None).await;
    let instance = first.licensing.instance_id().to_string();
    let app = app(db, &dir, Some(key(&instance, Kind::Perpetual, None))).await;
    let admin = bearer(&app, db, "boss", Role::Admin).await;

    let (_, licence) = call(&app, Method::GET, "/api/v2/licence", Some(&admin), None).await;
    assert_eq!(licence["state"], "active");
    assert_eq!(licence["from_environment"], true);
    assert_eq!(licence["licence"]["expires_at"], Value::Null);
    let (status, _) = call(&app, Method::DELETE, "/api/v2/licence", Some(&admin), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A bad key in the environment is not fatal: the instance runs as
    // Community.
    let broken = self::app(db, &dir, Some("HALOID1.nonsense.x".into())).await;
    assert!(broken.licensing.entitlements(Utc::now()).branding);

    scratch.drop_db().await;
}
