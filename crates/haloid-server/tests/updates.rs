//! The updater over HTTP: who may see and drive it, and what a build that
//! cannot update itself says.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::Utc;
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_release::{Edition, Manifest};
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig, ProConfig};
use haloid_service::updates::{Installer, Source, UpdatesConfig};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SIGNER: [u8; 32] = [8; 32];
const TARGET: &str = "x86_64-unknown-linux-musl";

async fn app(db: &haloid_db::Db, dir: &TempDir, updates: Option<UpdatesConfig>) -> Arc<App> {
    Arc::new(
        App::start_default(
            db.clone(),
            AppConfig {
                data_dir: dir.path().to_path_buf(),
                altcha_secret: "test".into(),
                pinned_settings: BTreeMap::new(),
                pro: ProConfig::default(),
                altcha: None,
                updates,
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

async fn call(app: &Arc<App>, method: Method, path: &str, auth: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, auth)
        .body(Body::empty())
        .unwrap();
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

#[tokio::test]
async fn administrators_see_the_release_on_offer_and_nobody_else_does() {
    let scratch = migrated_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let host = MockServer::start().await;
    let manifest = Manifest {
        version: "9.0.0".into(),
        edition: Edition::Community,
        target: TARGET.into(),
        size: 3,
        sha256: "ab".repeat(32),
        url: format!("{}/haloid-server", host.uri()),
        notes_url: None,
        published_at: Utc::now(),
    };
    let name = format!("haloid-server-community-{TARGET}.manifest");
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "assets": [{"name": name, "browser_download_url": format!("{}/m", host.uri())}]
        })))
        .mount(&host)
        .await;
    Mock::given(method("GET"))
        .and(path("/m"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(haloid_release::sign(&manifest, &SIGNER)),
        )
        .mount(&host)
        .await;
    let app = app(
        &scratch.pool,
        &dir,
        Some(UpdatesConfig {
            edition: Edition::Community,
            target: TARGET.into(),
            current: "0.1.0".into(),
            // Not installable: the page offers the notice only.
            installer: Installer::Binary { exe: None },
            source: Source::Github { api: host.uri() },
            trusted: vec![haloid_release::public_key(&SIGNER)],
            disabled: None,
        }),
    )
    .await;
    let admin = bearer(&app, &scratch.pool, "root", Role::Admin).await;
    let member = bearer(&app, &scratch.pool, "mira", Role::User).await;

    let (status, _) = call(&app, Method::GET, "/api/v2/updates", &member).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&app, Method::POST, "/api/v2/updates/check", &member).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        host.received_requests().await.unwrap().is_empty(),
        "a member made it look"
    );

    let (status, view) = call(&app, Method::GET, "/api/v2/updates", &admin).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["enabled"], true);
    assert_eq!(view["mode"], "notify");
    assert_eq!(view["available"], Value::Null);

    let (status, view) = call(&app, Method::POST, "/api/v2/updates/check", &admin).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["available"]["version"], "9.0.0");
    assert_eq!(view["installable"], false);
    assert!(view["not_installable"].as_str().unwrap().contains("binary"));

    let (status, _) = call(&app, Method::POST, "/api/v2/updates/install", &admin).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    scratch.drop_db().await;
}

#[tokio::test]
async fn a_build_without_an_updater_says_so() {
    let scratch = migrated_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let app = app(&scratch.pool, &dir, None).await;
    let admin = bearer(&app, &scratch.pool, "root", Role::Admin).await;
    let (status, view) = call(&app, Method::GET, "/api/v2/updates", &admin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["enabled"], false);
    assert!(view["disabled_reason"].is_string());
    scratch.drop_db().await;
}
