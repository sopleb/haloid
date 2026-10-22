//! Moderation and administration over HTTP, and the audit trail it leaves.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::Utc;
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const BOUNDARY: &str = "haloidadminboundary";

async fn app(db: &haloid_db::Db, dir: &TempDir) -> Arc<App> {
    Arc::new(
        App::start_default(
            db.clone(),
            AppConfig {
                data_dir: dir.path().to_path_buf(),
                altcha_secret: "test".into(),
                pinned_settings: BTreeMap::from([(
                    "site.maintenance".to_string(),
                    "false".to_string(),
                )]),
                pro: haloid_service::app::ProConfig::default(),
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
    let response = router(app.clone())
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice::<Value>(&bytes).unwrap()["file"].clone()
}

fn path_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.find('/').map_or("/".into(), |i| rest[i..].to_string())
}

#[tokio::test]
async fn moderators_approve_and_deny_held_files() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let admin = bearer(&app, &scratch.pool, "root", Role::Admin).await;
    let moderator = bearer(&app, &scratch.pool, "mod", Role::Manager).await;
    let user = bearer(&app, &scratch.pool, "alice", Role::User).await;
    let (status, _) = call(
        &app,
        Method::PATCH,
        "/api/v2/settings",
        Some(&admin),
        Some(json!({"moderation.mode": "guests"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let kept = upload(&app, None, "still.png").await;
    let refused = upload(&app, None, "still.jpg").await;
    assert_eq!(kept["approved"], false);
    let kept_id = kept["id"].as_str().unwrap();
    let image = path_of(kept["urls"]["image"].as_str().unwrap());
    let (status, _) = call(&app, Method::GET, &image, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "held bytes are not served");

    let (status, _) = call(
        &app,
        Method::GET,
        "/api/v2/moderation/queue",
        Some(&user),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, queue) = call(
        &app,
        Method::GET,
        "/api/v2/moderation/queue",
        Some(&moderator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    let ids: Vec<&str> = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["file"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![kept_id, refused["id"].as_str().unwrap()]);

    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v2/moderation/{kept_id}/approve"),
        Some(&moderator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, shown) = call(
        &app,
        Method::GET,
        &format!("/api/v2/files/{kept_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        (status, shown["approved"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    let (status, _) = call(&app, Method::GET, &image, None, None).await;
    assert_eq!(status, StatusCode::OK, "approval releases the bytes");

    let refused_id = refused["id"].as_str().unwrap();
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v2/moderation/{refused_id}/deny"),
        Some(&moderator),
        Some(json!({"note": "spam"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let row = haloid_db::files::find_by_id_encoded(&scratch.pool, refused_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.deletion.map(|d| d.reason),
        Some(haloid_core::files::DeleteReason::Moderation)
    );
    let (_, queue) = call(
        &app,
        Method::GET,
        "/api/v2/moderation/queue",
        Some(&moderator),
        None,
    )
    .await;
    assert_eq!(queue["items"], json!([]));

    let (status, log) = call(&app, Method::GET, "/api/v2/admin/audit", Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    let actions: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        vec!["file.deny", "file.approve", "settings.update"],
        "newest first"
    );
    assert_eq!(log[0]["actor"], "mod");
    assert_eq!(log[0]["detail"]["note"], "spam");

    scratch.drop_db().await;
}

#[tokio::test]
async fn settings_are_written_whole_or_not_at_all() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let admin = bearer(&app, &scratch.pool, "root", Role::Admin).await;
    let moderator = bearer(&app, &scratch.pool, "mod", Role::Manager).await;

    let (status, _) = call(
        &app,
        Method::GET,
        "/api/v2/settings",
        Some(&moderator),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "settings are for administrators"
    );
    let (status, list) = call(&app, Method::GET, "/api/v2/settings", Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    let find = |list: &Value, key: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|s| s["key"] == key)
            .cloned()
            .unwrap()
    };
    assert_eq!(find(&list, "site.name")["value"], "Haloid");
    assert_eq!(find(&list, "site.maintenance")["pinned"], true);

    let (status, list) = call(
        &app,
        Method::PATCH,
        "/api/v2/settings",
        Some(&admin),
        Some(json!({"site.name": "Pics"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(find(&list, "site.name")["value"], "Pics");
    let (_, config) = call(&app, Method::GET, "/api/v2/config", None, None).await;
    assert_eq!(config["site_name"], "Pics", "live at once");

    for bad in [
        json!({"site.name": "Other", "no.such.key": "x"}),
        json!({"site.name": "Other", "uploads.max_size_user": "lots"}),
        json!({"site.name": "Other", "site.maintenance": "true"}),
    ] {
        let (status, problem) = call(
            &app,
            Method::PATCH,
            "/api/v2/settings",
            Some(&admin),
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {problem}");
    }
    let (_, config) = call(&app, Method::GET, "/api/v2/config", None, None).await;
    assert_eq!(
        config["site_name"], "Pics",
        "a refused write changes nothing"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn administrators_manage_users_storages_and_deleted_files() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let admin = bearer(&app, &scratch.pool, "root", Role::Admin).await;
    let alice = bearer(&app, &scratch.pool, "alice", Role::User).await;
    bearer(&app, &scratch.pool, "bob", Role::User).await;

    // Users: search, promote, ban — but never yourself.
    let (status, page) = call(
        &app,
        Method::GET,
        "/api/v2/admin/users?search=ALI",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let alice_id = items[0]["id"].as_str().unwrap().to_string();
    let (status, user) = call(
        &app,
        Method::PATCH,
        &format!("/api/v2/admin/users/{alice_id}"),
        Some(&admin),
        Some(json!({"role": "manager"})),
    )
    .await;
    assert_eq!(
        (status, user["role"].as_str()),
        (StatusCode::OK, Some("manager"))
    );
    let (status, _) = call(
        &app,
        Method::PATCH,
        &format!("/api/v2/admin/users/{alice_id}"),
        Some(&admin),
        Some(json!({"status": "banned"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, Method::GET, "/api/v2/auth/me", Some(&alice), None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a banned user's key stops working"
    );
    let (_, me) = call(
        &app,
        Method::GET,
        "/api/v2/admin/users?search=root",
        Some(&admin),
        None,
    )
    .await;
    let root_id = me["items"][0]["id"].as_str().unwrap().to_string();
    let (status, _) = call(
        &app,
        Method::PATCH,
        &format!("/api/v2/admin/users/{root_id}"),
        Some(&admin),
        Some(json!({"role": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Storages: add, edit, retire.
    let (status, created) = call(
        &app,
        Method::POST,
        "/api/v2/storages",
        Some(&admin),
        Some(json!({"name": "archive", "kind": "local", "config": {}, "url_prefix": "https://cdn.example.com/archive/"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["url_prefix"], "https://cdn.example.com/archive");
    let id = created["id"].as_u64().unwrap();
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v2/storages",
        Some(&admin),
        Some(json!({"name": "archive", "kind": "local", "config": {}, "url_prefix": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v2/storages",
        Some(&admin),
        Some(json!({"name": "x", "kind": "ftp", "config": {}, "url_prefix": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, updated) = call(
        &app,
        Method::PATCH,
        &format!("/api/v2/storages/{id}"),
        Some(&admin),
        Some(json!({"name": "archive", "kind": "local", "config": {}, "url_prefix": "https://cdn.example.com/a", "priority": 5})),
    )
    .await;
    assert_eq!(
        (status, updated["priority"].as_i64()),
        (StatusCode::OK, Some(5))
    );
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v2/storages/{id}/retire"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = call(&app, Method::GET, "/api/v2/storages", Some(&admin), None).await;
    let archive = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap();
    assert!(archive["retired_at"].is_string());

    // Deleted files: listed, restored, then purged by the job.
    let file = upload(&app, Some(&admin), "still.png").await;
    let file_id = file["id"].as_str().unwrap().to_string();
    call(
        &app,
        Method::DELETE,
        &format!("/api/v2/files/{file_id}"),
        Some(&admin),
        None,
    )
    .await;
    let (_, deleted) = call(
        &app,
        Method::GET,
        "/api/v2/admin/files/deleted",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(deleted["items"][0]["id"], file_id.as_str());
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v2/admin/files/{file_id}/restore"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/api/v2/files/{file_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "restored");

    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v2/admin/files/{file_id}/purge"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let runner = haloid_jobs::Runner::new(app.clone());
    assert!(runner.run_once(Utc::now()).await.unwrap());
    assert!(
        haloid_db::files::find_by_id_encoded(&scratch.pool, &file_id)
            .await
            .unwrap()
            .is_none()
    );

    let (status, _) = call(
        &app,
        Method::GET,
        "/api/v2/admin/files/deleted",
        Some(&alice),
        None,
    )
    .await;
    assert_ne!(status, StatusCode::OK);

    scratch.drop_db().await;
}
