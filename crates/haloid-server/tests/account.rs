//! The account area's endpoints, and password albums, over HTTP.

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

const BOUNDARY: &str = "haloidaccountboundary";
const ORIGIN: &str = "http://localhost:8080";

async fn app(db: &haloid_db::Db, dir: &TempDir) -> Arc<App> {
    Arc::new(
        App::start_default(
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
        .unwrap(),
    )
}

async fn user(
    db: &haloid_db::Db,
    name: &str,
    password: Option<&str>,
) -> haloid_core::account::User {
    let hash = password.map(|p| secrets::hash_password(p).unwrap());
    haloid_db::users::create(
        db,
        &haloid_db::users::NewUser {
            username: name,
            email: None,
            password_hash: hash.as_deref(),
            role: Role::User,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap()
}

async fn session(db: &haloid_db::Db, user: &haloid_core::account::User) -> String {
    let token = secrets::new_session_token();
    haloid_db::sessions::create(
        db,
        &haloid_db::sessions::NewSession {
            user_id: user.id,
            token_hash: &secrets::sha256_hex(&token),
            ip: None,
            user_agent: Some("test"),
            expires_at: Utc::now() + Duration::days(1),
        },
    )
    .await
    .unwrap();
    format!("__Host-haloid_session={token}")
}

struct Call<'a> {
    method: Method,
    path: &'a str,
    bearer: Option<&'a str>,
    cookie: Option<&'a str>,
    ours: bool,
    body: Option<Value>,
}

impl<'a> Call<'a> {
    fn new(method: Method, path: &'a str) -> Self {
        Self {
            method,
            path,
            bearer: None,
            cookie: None,
            ours: false,
            body: None,
        }
    }
    fn bearer(mut self, b: &'a str) -> Self {
        self.bearer = Some(b);
        self
    }
    fn cookie(mut self, c: &'a str) -> Self {
        self.cookie = Some(c);
        self.ours = true;
        self
    }
    fn ours(mut self) -> Self {
        self.ours = true;
        self
    }
    fn json(mut self, v: Value) -> Self {
        self.body = Some(v);
        self
    }
    async fn send(self, app: &Arc<App>) -> (StatusCode, Option<String>, Value) {
        let mut builder = Request::builder().method(self.method).uri(self.path);
        if let Some(b) = self.bearer {
            builder = builder.header(header::AUTHORIZATION, b);
        }
        if let Some(c) = self.cookie {
            builder = builder.header(header::COOKIE, c);
        }
        if self.ours {
            builder = builder
                .header("x-requested-with", "XMLHttpRequest")
                .header(header::ORIGIN, ORIGIN);
        }
        let body = match self.body {
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
}

#[tokio::test]
async fn a_user_edits_their_profile_and_manages_keys() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let alice = user(&scratch.pool, "alice", None).await;
    let bob = user(&scratch.pool, "bob", None).await;
    let (_, key) = app
        .auth
        .create_api_key(Some(alice.id), "first")
        .await
        .unwrap();
    let alice_key = format!("Bearer {key}");
    let (_, key) = app
        .auth
        .create_api_key(Some(bob.id), "bob's")
        .await
        .unwrap();
    let bob_key = format!("Bearer {key}");

    let (status, _, me) = Call::new(Method::PATCH, "/api/v2/auth/me")
        .bearer(&alice_key)
        .json(json!({"display_name": "  Alice A.  ", "email": "a@example.com", "is_private": true}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(
        (me["display_name"].as_str(), me["is_private"].as_bool()),
        (Some("Alice A."), Some(true))
    );
    let (status, _, _) = Call::new(Method::GET, "/api/v2/users/alice")
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a private profile is hidden");
    let (status, _, _) = Call::new(Method::PATCH, "/api/v2/auth/me")
        .bearer(&bob_key)
        .json(json!({"email": "A@example.com"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "someone else's email");
    let (status, _, _) = Call::new(Method::PATCH, "/api/v2/auth/me")
        .json(json!({"display_name": "x"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A new key works at once, is listed without its secret, and stops
    // working when revoked. Nobody else can revoke it.
    let (status, _, created) = Call::new(Method::POST, "/api/v2/auth/api-keys")
        .bearer(&alice_key)
        .json(json!({"label": "sharex"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let secret = created["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("hld_"));
    let new_key = format!("Bearer {secret}");
    let (status, _, listed) = Call::new(Method::GET, "/api/v2/auth/api-keys")
        .bearer(&new_key)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().unwrap().len(), 2);
    assert!(
        !listed.to_string().contains(&secret),
        "the secret is never listed"
    );

    let id = created["key"]["id"].as_u64().unwrap();
    let path = format!("/api/v2/auth/api-keys/{id}");
    let (status, _, _) = Call::new(Method::DELETE, &path)
        .bearer(&bob_key)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = Call::new(Method::DELETE, &path)
        .bearer(&alice_key)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, problem) = Call::new(Method::GET, "/api/v2/auth/me")
        .bearer(&new_key)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{problem}");

    scratch.drop_db().await;
}

#[tokio::test]
async fn sessions_are_listed_revoked_and_cleared_by_a_password_change() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let alice = user(&scratch.pool, "alice", Some("first password here")).await;
    let laptop = session(&scratch.pool, &alice).await;
    let phone = session(&scratch.pool, &alice).await;
    let tablet = session(&scratch.pool, &alice).await;

    let (status, _, list) = Call::new(Method::GET, "/api/v2/auth/sessions")
        .cookie(&laptop)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap().clone();
    assert_eq!(list.len(), 3);
    assert_eq!(list.iter().filter(|s| s["current"] == true).count(), 1);

    // Sign the phone out from the laptop.
    let phone_id = {
        let (_, _, theirs) = Call::new(Method::GET, "/api/v2/auth/sessions")
            .cookie(&phone)
            .send(&app)
            .await;
        theirs
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["current"] == true)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (status, _, _) = Call::new(Method::DELETE, &format!("/api/v2/auth/sessions/{phone_id}"))
        .cookie(&laptop)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = Call::new(Method::GET, "/api/v2/auth/me")
        .cookie(&phone)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A wrong current password changes nothing; the right one signs the
    // tablet out and keeps the laptop.
    let (status, _, _) = Call::new(Method::POST, "/api/v2/auth/password")
        .cookie(&laptop)
        .json(json!({"current": "not it", "new": "second password here"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, problem) = Call::new(Method::POST, "/api/v2/auth/password")
        .cookie(&laptop)
        .json(json!({"current": "first password here", "new": "second password here"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{problem}");
    let (status, _, _) = Call::new(Method::GET, "/api/v2/auth/me")
        .cookie(&tablet)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = Call::new(Method::GET, "/api/v2/auth/me")
        .cookie(&laptop)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::OK);

    // An API key cannot change the password.
    let (_, key) = app.auth.create_api_key(Some(alice.id), "k").await.unwrap();
    let (status, _, _) = Call::new(Method::POST, "/api/v2/auth/password")
        .bearer(&format!("Bearer {key}"))
        .ours()
        .json(json!({"current": "second password here", "new": "third password here"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_password_album_opens_with_its_password_until_it_changes() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = app(&scratch.pool, &dir).await;
    let owner = user(&scratch.pool, "owner", None).await;
    let (_, key) = app.auth.create_api_key(Some(owner.id), "k").await.unwrap();
    let bearer = format!("Bearer {key}");

    let (_, _, album) = Call::new(Method::POST, "/api/v2/albums")
        .bearer(&bearer)
        .json(json!({"name": "Locked", "privacy": "password", "password": "open sesame"}))
        .send(&app)
        .await;
    let album_id = album["id"].as_str().unwrap().to_string();
    let bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/still.png"))
            .unwrap();
    let mut upload = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"album\"\r\n\r\n{album_id}\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"still.png\"\r\nContent-Type: image/png\r\n\r\n"
    )
    .into_bytes();
    upload.extend_from_slice(&bytes);
    upload.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let response = router(app.clone())
        .oneshot(
            Request::post("/api/v2/uploads")
                .header(header::AUTHORIZATION, &bearer)
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .body(Body::from(upload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let file_path = format!("/api/v2/files/{}", created["file"]["id"].as_str().unwrap());
    let album_path = format!("/api/v2/albums/{album_id}");
    let unlock_path = format!("{album_path}/unlock");

    // Locked: the album says so, its files do not show.
    let (status, _, problem) = Call::new(Method::GET, &album_path).send(&app).await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("album_locked"))
    );
    let (status, _, _) = Call::new(Method::GET, &format!("{album_path}/files"))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = Call::new(Method::GET, &file_path).send(&app).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, _) = Call::new(Method::POST, &unlock_path)
        .ours()
        .json(json!({"password": "wrong"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = Call::new(Method::POST, &unlock_path)
        .json(json!({"password": "open sesame"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "not from our page");
    let (status, set_cookie, _) = Call::new(Method::POST, &unlock_path)
        .ours()
        .json(json!({"password": "open sesame"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let set_cookie = set_cookie.unwrap();
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    let grant = set_cookie.split(';').next().unwrap().to_string();

    for path in [&album_path, &format!("{album_path}/files"), &file_path] {
        let (status, _, body) = Call::new(Method::GET, path).cookie(&grant).send(&app).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
    // A forged grant opens nothing.
    let forged = format!("haloid_album_{album_id}=9999999999.{}", "0".repeat(64));
    let (status, _, _) = Call::new(Method::GET, &album_path)
        .cookie(&forged)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A new password voids the old grant.
    let (status, _, _) = Call::new(Method::PATCH, &album_path)
        .bearer(&bearer)
        .json(json!({"password": "new secret"}))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = Call::new(Method::GET, &album_path)
        .cookie(&grant)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    scratch.drop_db().await;
}
