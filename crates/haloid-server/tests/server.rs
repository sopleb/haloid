//! The assembled server, end to end over HTTP: a real MariaDB, the real
//! service, and requests through the router as a client would send them.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use haloid_core::auth::Actor;
use haloid_media::{MediaConfig, MediaToolkit};
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig};
use haloid_service::files::DeleteAs;
use haloid_service::upload::{UploadCommand, UploadSource};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

async fn app(db: &haloid_db::Db, data: &TempDir) -> Arc<App> {
    Arc::new(
        App::start(
            db.clone(),
            Arc::new(MediaToolkit::new(MediaConfig::default())),
            AppConfig {
                data_dir: data.path().to_path_buf(),
                altcha_secret: "test".into(),
                pinned_settings: BTreeMap::new(),
                pro: haloid_service::app::ProConfig::default(),
                altcha: None,
                updates: None,
            },
        )
        .await
        .expect("the service assembles"),
    )
}

async fn get(app: &Arc<App>, path: &str) -> axum::response::Response {
    router(app.clone())
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body(response: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn health_and_readiness_answer() {
    let scratch = migrated_or_skip!();
    let data = TempDir::new().unwrap();
    let app = app(&scratch.pool, &data).await;

    let health = get(&app, "/api/v2/healthz").await;
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(body(health).await, br#"{"status":"ok"}"#);

    let ready = get(&app, "/api/v2/readyz").await;
    assert_eq!(ready.status(), StatusCode::OK);

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_fresh_install_gets_a_working_local_storage() {
    let scratch = migrated_or_skip!();
    let data = TempDir::new().unwrap();
    let app = app(&scratch.pool, &data).await;

    let storages = haloid_db::storages::list(&scratch.pool).await.unwrap();
    assert_eq!(storages.len(), 1);
    assert_eq!(storages[0].name, "main");
    assert_eq!(storages[0].url_prefix, "http://localhost:8080/files");

    // A second start finds it rather than adding another.
    drop(app);
    let _again = self::app(&scratch.pool, &data).await;
    assert_eq!(
        haloid_db::storages::list(&scratch.pool)
            .await
            .unwrap()
            .len(),
        1
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn uploaded_files_are_served_until_deleted() {
    let scratch = migrated_or_skip!();
    let data = TempDir::new().unwrap();
    let app = app(&scratch.pool, &data).await;

    let bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/still.png"))
            .unwrap();
    let uploaded = app
        .uploads
        .upload(UploadCommand {
            actor: Actor::Guest,
            source: UploadSource::Stream {
                reader: Box::new(std::io::Cursor::new(bytes.clone())),
                filename: Some("still.png".into()),
            },
            title: None,
            description: None,
            tags: Vec::new(),
            album: None,
            category: None,
            nsfw: None,
            expiration: None,
            width: None,
            uploader_ip: None,
        })
        .await
        .unwrap();

    let url = &uploaded.urls.derivatives["image"];
    let path = url.strip_prefix("http://localhost:8080").unwrap();
    let served = get(&app, path).await;
    assert_eq!(served.status(), StatusCode::OK, "{path}");
    assert_eq!(served.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(served.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(served.headers().contains_key(header::CACHE_CONTROL));
    let served_bytes = body(served).await;
    assert!(!served_bytes.is_empty());

    let thumb = uploaded.urls.derivatives["thumb"]
        .strip_prefix("http://localhost:8080")
        .unwrap()
        .to_string();
    assert_eq!(get(&app, &thumb).await.status(), StatusCode::OK);

    let token = uploaded
        .urls
        .delete
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    app.files
        .delete(
            uploaded.file.id_encoded.as_str(),
            DeleteAs::Token(&token),
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(
        get(&app, path).await.status(),
        StatusCode::NOT_FOUND,
        "a deleted file is still served"
    );
    assert_eq!(get(&app, &thumb).await.status(), StatusCode::NOT_FOUND);

    scratch.drop_db().await;
}

#[tokio::test]
async fn nothing_outside_the_served_tree_is_reachable() {
    let scratch = migrated_or_skip!();
    let data = TempDir::new().unwrap();
    let app = app(&scratch.pool, &data).await;

    let served = data.path().join("storage/main");
    std::fs::create_dir_all(&served).unwrap();
    std::fs::write(served.join(".cat.jpg.123.0.tmp"), b"half written").unwrap();
    std::fs::write(data.path().join("secret.txt"), b"outside").unwrap();
    std::fs::create_dir_all(data.path().join("quarantine/main")).unwrap();
    std::fs::write(data.path().join("quarantine/main/gone.jpg"), b"deleted").unwrap();

    for path in [
        "/files/.cat.jpg.123.0.tmp",
        "/files/../secret.txt",
        "/files/%2e%2e/secret.txt",
        "/files/../quarantine/main/gone.jpg",
        "/quarantine/main/gone.jpg",
        "/files/",
    ] {
        let status = get(&app, path).await.status();
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST,
            "{path} answered {status}"
        );
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn the_spa_answers_every_page_but_never_an_api_path() {
    let scratch = migrated_or_skip!();
    let data = TempDir::new().unwrap();
    let app = app(&scratch.pool, &data).await;

    let site = TempDir::new().unwrap();
    std::fs::write(
        site.path().join("index.html"),
        "<!doctype html><title>spa</title>",
    )
    .unwrap();
    std::fs::create_dir_all(site.path().join("_app/immutable")).unwrap();
    std::fs::write(site.path().join("_app/immutable/app.js"), "console.log(1)").unwrap();

    let request = |path: &str| Request::get(path).body(Body::empty()).unwrap();
    let full = || haloid_server::router::app_router(app.clone(), Some(site.path()));

    // A client-side route the server knows nothing about gets the shell.
    let page = full().oneshot(request("/f/Ab3dE5fG7h")).await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CACHE_CONTROL], "no-cache");
    assert!(body(page).await.starts_with(b"<!doctype html>"));

    let asset = full()
        .oneshot(request("/_app/immutable/app.js"))
        .await
        .unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
    assert!(
        asset.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable")
    );

    // An API path that does not exist is a readable 404, not a web page.
    let missing = full()
        .oneshot(request("/api/v2/no-such-thing"))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        missing.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );

    // Real endpoints still win over the fallback.
    let health = full().oneshot(request("/api/v2/healthz")).await.unwrap();
    assert_eq!(body(health).await, br#"{"status":"ok"}"#);

    scratch.drop_db().await;
}

#[tokio::test]
async fn public_pages_carry_link_previews_for_what_a_guest_may_see() {
    let scratch = migrated_or_skip!();
    let data = TempDir::new().unwrap();
    let app = app(&scratch.pool, &data).await;
    let site = TempDir::new().unwrap();
    std::fs::write(
        site.path().join("index.html"),
        "<!doctype html><head><!--%haloid.head%--></head><body></body>",
    )
    .unwrap();

    let user = haloid_db::users::create(
        &scratch.pool,
        &haloid_db::users::NewUser {
            username: "dana",
            email: None,
            password_hash: None,
            role: haloid_core::auth::Role::User,
            status: haloid_core::account::UserStatus::Valid,
        },
    )
    .await
    .unwrap();
    let bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/still.jpg"))
            .unwrap();
    let shown = app
        .uploads
        .upload(UploadCommand {
            actor: user.actor(),
            source: UploadSource::Stream {
                reader: Box::new(std::io::Cursor::new(bytes)),
                filename: Some("still.jpg".into()),
            },
            title: Some(r#"Cat "on" <a> mat"#.into()),
            description: Some("A cat.".into()),
            tags: Vec::new(),
            album: None,
            category: None,
            nsfw: None,
            expiration: None,
            width: None,
            uploader_ip: None,
        })
        .await
        .unwrap();

    let page = |path: String| {
        let app = app.clone();
        let dir = site.path().to_path_buf();
        async move {
            let response = haloid_server::router::app_router(app, Some(&dir))
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            String::from_utf8(body(response).await).unwrap()
        }
    };

    let html = page(format!("/f/{}", shown.file.id_encoded)).await;
    assert!(
        html.contains(r#"<meta property="og:title" content="Cat &quot;on&quot; &lt;a&gt; mat" />"#),
        "{html}"
    );
    assert!(!html.contains("<a> mat"), "a raw title reached the page");
    assert!(html.contains(r#"property="og:image""#));
    assert!(html.contains(r#"property="og:image:width" content="32""#));
    assert!(html.contains(r#"name="twitter:card" content="summary_large_image""#));
    assert!(html.contains(r#"<link rel="canonical""#));
    assert!(html.contains(r#""@type":"ImageObject""#));
    assert!(
        !html.contains("<!--%haloid.head%-->"),
        "the marker was left in"
    );

    let profile = page("/u/dana".into()).await;
    assert!(profile.contains(r#"property="og:type" content="profile""#));

    // Hold it for moderation: a guest may no longer see it, so no preview.
    haloid_db::files::set_approved(&scratch.pool, shown.file.id, false)
        .await
        .unwrap();
    let held = page(format!("/f/{}", shown.file.id_encoded)).await;
    assert!(
        !held.contains("og:title"),
        "an unapproved file got a preview"
    );

    // An unknown id still gets the app, which shows its own not-found.
    let unknown = page("/f/0000000000".into()).await;
    assert!(unknown.starts_with("<!doctype html>") && !unknown.contains("og:"));

    scratch.drop_db().await;
}

/// The frontend compiled into the binary serves like the one on disk: the
/// shell for client routes, hashed assets cached for good, head tags on
/// public pages, and API paths never answered with a web page.
#[cfg(feature = "embed-frontend")]
#[tokio::test]
async fn the_embedded_frontend_serves_like_the_one_on_disk() {
    use haloid_server::router::{Frontend, app_router_with};
    let scratch = migrated_or_skip!();
    let data = TempDir::new().unwrap();
    let app = app(&scratch.pool, &data).await;
    let full = || app_router_with(app.clone(), Frontend::Embedded);
    let request = |path: &str| Request::get(path).body(Body::empty()).unwrap();

    let page = full().oneshot(request("/explore")).await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert!(
        page.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert_eq!(page.headers()[header::CACHE_CONTROL], "no-cache");
    let html = String::from_utf8(body(page).await.to_vec()).unwrap();
    assert!(html.contains("_app/immutable/"), "the built shell");

    let asset = html
        .split('"')
        .find(|s| s.contains("_app/immutable/") && s.ends_with(".js"))
        .expect("the shell links its scripts")
        .trim_start_matches('.')
        .to_string();
    let asset = if asset.starts_with('/') {
        asset
    } else {
        format!("/{asset}")
    };
    let script = full().oneshot(request(&asset)).await.unwrap();
    assert_eq!(script.status(), StatusCode::OK, "{asset}");
    assert!(
        script.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript")
    );
    assert!(
        script.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable")
    );

    let missing = full()
        .oneshot(request("/api/v2/no-such-thing"))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    // And the API itself still answers: the asset fallback is only that.
    for path in ["/api/v2/healthz", "/api/v2/readyz", "/api/v2/config"] {
        let ok = full().oneshot(request(path)).await.unwrap();
        assert_eq!(ok.status(), StatusCode::OK, "{path}");
    }

    scratch.drop_db().await;
}
