//! Upload rate limits, on both APIs, and the metrics endpoint.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const BOUNDARY: &str = "haloidhardeningboundary";

async fn app(db: &haloid_db::Db, dir: &TempDir) -> Arc<App> {
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
    app.settings
        .update(&BTreeMap::from([
            ("uploads.per_hour_guest".to_string(), "2".to_string()),
            ("uploads.per_hour_user".to_string(), "2".to_string()),
        ]))
        .await
        .unwrap();
    Arc::new(app)
}

async fn key(app: &App, db: &haloid_db::Db, name: &str, role: Role) -> String {
    let user = haloid_db::users::create(
        db,
        &haloid_db::users::NewUser {
            username: name,
            email: None,
            password_hash: None,
            role,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap();
    app.auth.create_api_key(Some(user.id), "k").await.unwrap().1
}

fn form(parts: &[(&str, &str)]) -> Vec<u8> {
    let png =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/still.png"))
            .unwrap();
    let mut body = Vec::new();
    for (name, value) in parts {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    let field = if parts.iter().any(|(n, _)| *n == "key") {
        "source"
    } else {
        "file"
    };
    body.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

async fn post(
    app: &Arc<App>,
    path: &str,
    auth: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, String) {
    let mut builder = Request::post(path).header(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={BOUNDARY}"),
    );
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    let response = router(app.clone())
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get(app: &Arc<App>, path: &str, auth: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::get(path);
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    let response = router(app.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn uploads_are_rate_limited_on_both_apis_but_not_for_staff() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;

    // Guests, over v2: two, then 429.
    for _ in 0..2 {
        let (status, body) = post(&app, "/api/v2/uploads", None, form(&[])).await;
        assert!(status.is_success(), "{body}");
    }
    let (status, body) = post(&app, "/api/v2/uploads", None, form(&[])).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(body.contains("rate_limited"), "{body}");

    // A member, over the legacy API: the same count, its own envelope.
    let member = key(&app, &scratch.pool, "alice", Role::User).await;
    for _ in 0..2 {
        let (status, body) = post(&app, "/api/1/upload", None, form(&[("key", &member)])).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = post(&app, "/api/1/upload", None, form(&[("key", &member)])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Flooding detected"), "{body}");

    // A moderator is not limited.
    let staff = key(&app, &scratch.pool, "mod", Role::Manager).await;
    for _ in 0..4 {
        let (status, body) = post(&app, "/api/1/upload", None, form(&[("key", &staff)])).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn metrics_are_for_an_administrator_or_the_scraper_token() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let admin = format!(
        "Bearer {}",
        key(&app, &scratch.pool, "founder", Role::Admin).await
    );
    let member = format!(
        "Bearer {}",
        key(&app, &scratch.pool, "alice", Role::User).await
    );
    post(&app, "/api/v2/uploads", Some(&member), form(&[])).await;

    let (status, _) = get(&app, "/api/v2/metrics", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = get(&app, "/api/v2/metrics", Some(&member)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, text) = get(&app, "/api/v2/metrics", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("haloid_uploads_total{outcome=\"stored\"} 1"),
        "{text}"
    );
    assert!(
        text.contains("# TYPE haloid_http_responses_total counter"),
        "{text}"
    );
    assert!(text.contains("haloid_moderation_pending 0"), "{text}");

    app.metrics.set_token("scrape-me".into());
    let (status, _) = get(&app, "/api/v2/metrics", Some("Bearer scrape-me")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(&app, "/api/v2/metrics", Some("Bearer wrong")).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a wrong token is just a bad key"
    );

    scratch.drop_db().await;
}
