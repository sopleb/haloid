//! `POST /api/v2/storages/{id}/test` against the local storage, served for
//! real on a port so the public-URL check fetches the probe over HTTP.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

async fn bearer(app: &App, db: &haloid_db::Db, name: &str, role: Role) -> String {
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
    let (_, key) = app.auth.create_api_key(Some(user.id), "t").await.unwrap();
    format!("Bearer {key}")
}

async fn test_storage(app: &Arc<App>, id: u64, auth: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::post(format!("/api/v2/storages/{id}/test"));
    if let Some(auth) = auth {
        request = request.header(header::AUTHORIZATION, auth);
    }
    let response = router(app.clone())
        .oneshot(request.body(Body::empty()).unwrap())
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
async fn an_administrator_tests_a_storage_step_by_step() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    // A real listener, and the site's base URL pointing at it, so the
    // bootstrapped storage's public URLs are fetchable.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Arc::new(
        App::start_default(
            scratch.pool.clone(),
            AppConfig {
                data_dir: dir.path().to_path_buf(),
                altcha_secret: "test".into(),
                pinned_settings: BTreeMap::from([("site.base_url".to_string(), base.clone())]),
                pro: haloid_service::app::ProConfig::default(),
                altcha: None,
                updates: None,
            },
        )
        .await
        .unwrap(),
    );
    let served = router(app.clone());
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });

    let storage = haloid_db::storages::list(&scratch.pool).await.unwrap()[0]
        .id
        .0;
    let admin = bearer(&app, &scratch.pool, "root", Role::Admin).await;
    let user = bearer(&app, &scratch.pool, "alice", Role::User).await;

    let (status, result) = test_storage(&app, storage, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let names: Vec<&str> = result["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "write",
            "read",
            "public_url",
            "quarantine",
            "quarantine_private",
            "release",
            "delete"
        ]
    );
    assert_eq!(result["ok"], true, "{result:#}");

    // Nothing left behind.
    let leftovers: Vec<_> = walk(dir.path())
        .into_iter()
        .filter(|p| p.contains("haloid-test"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    let (status, _) = test_storage(&app, storage, Some(&user)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = test_storage(&app, storage, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = test_storage(&app, 9999, Some(&admin)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    scratch.drop_db().await;
}

fn walk(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path.display().to_string());
        }
    }
    out
}
