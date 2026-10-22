//! The watermark, end to end: set by an administrator, stamped on a guest's
//! upload, the untouched upload kept as the original. Checked by looking at
//! pixels, never by comparing bytes.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_server::router::router;
use haloid_service::app::{App, AppConfig};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const BOUNDARY: &str = "haloidwatermarkboundary";

fn vips(args: &[&str]) -> String {
    let out = Command::new("vips")
        .args(args)
        .env("VIPS_WARNING", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "vips {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A dark 800x600 JPEG, and a white PNG mark.
fn images(dir: &Path) -> (Vec<u8>, Vec<u8>) {
    let noise = dir.join("noise.tif");
    vips(&[
        "gaussnoise",
        noise.to_str().unwrap(),
        "800",
        "600",
        "--mean",
        "50",
        "--sigma",
        "10",
    ]);
    let cast = dir.join("cast.tif");
    vips(&[
        "cast",
        noise.to_str().unwrap(),
        cast.to_str().unwrap(),
        "uchar",
    ]);
    let photo = dir.join("photo.jpg");
    vips(&["copy", cast.to_str().unwrap(), photo.to_str().unwrap()]);
    let black = dir.join("black.tif");
    vips(&[
        "black",
        black.to_str().unwrap(),
        "200",
        "100",
        "--bands",
        "3",
    ]);
    let mark = dir.join("mark.png");
    vips(&[
        "linear",
        black.to_str().unwrap(),
        mark.to_str().unwrap(),
        "1",
        "255",
        "--uchar",
    ]);
    (std::fs::read(photo).unwrap(), std::fs::read(mark).unwrap())
}

fn corner_brightness(dir: &Path, bytes: &[u8]) -> f64 {
    let path = dir.join(format!("served-{}.jpg", rand_suffix()));
    std::fs::write(&path, bytes).unwrap();
    let region = path.with_extension("region.tif");
    vips(&[
        "crop",
        path.to_str().unwrap(),
        region.to_str().unwrap(),
        "610",
        "500",
        "160",
        "60",
    ]);
    vips(&["avg", region.to_str().unwrap()]).parse().unwrap()
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

async fn send(app: &Arc<App>, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = router(app.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

async fn upload(app: &Arc<App>, bytes: &[u8]) -> Value {
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"photo.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let (status, body) = send(
        app,
        Request::post("/api/v2/uploads")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    serde_json::from_slice::<Value>(&body).unwrap()["file"].clone()
}

fn path_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.find('/').map_or("/".into(), |i| rest[i..].to_string())
}

#[tokio::test]
async fn an_administrators_mark_is_stamped_on_guest_uploads() {
    let scratch = migrated_or_skip!();
    let dir = TempDir::new().unwrap();
    let app = Arc::new(
        App::start_default(
            scratch.pool.clone(),
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
    );
    let admin = haloid_db::users::create(
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
    let (_, key) = app.auth.create_api_key(Some(admin.id), "k").await.unwrap();
    let bearer = format!("Bearer {key}");
    let (photo, mark) = images(dir.path());

    // Only an image is accepted as the mark, and only from an admin.
    let put = |body: Vec<u8>, auth: Option<&str>| {
        let mut builder = Request::builder()
            .method(Method::PUT)
            .uri("/api/v2/settings/watermark");
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder.body(Body::from(body)).unwrap()
    };
    let (status, _) = send(&app, put(b"not an image".to_vec(), Some(&bearer))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = send(&app, put(mark.clone(), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&app, put(mark.clone(), Some(&bearer))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Set but not enabled: nothing is stamped.
    let plain = upload(&app, &photo).await;
    let (_, served) = send(
        &app,
        Request::get(path_of(plain["urls"]["image"].as_str().unwrap()))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let base = corner_brightness(dir.path(), &served);

    let settings = json!({
        "watermark.enabled": "true",
        "watermark.percentage": "25",
        "watermark.opacity": "90",
        "watermark.margin": "10",
        "watermark.min_width": "100",
        "watermark.min_height": "100",
        "watermark.position": "bottom-right",
        "uploads.detect_duplicates": "false"
    });
    let (status, body) = send(
        &app,
        Request::builder()
            .method(Method::PATCH)
            .uri("/api/v2/settings")
            .header(header::AUTHORIZATION, &bearer)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(settings.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let marked = upload(&app, &photo).await;
    assert!(
        marked["chain"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "original"),
        "the untouched upload is kept: {marked}"
    );
    let (status, served) = send(
        &app,
        Request::get(path_of(marked["urls"]["image"].as_str().unwrap()))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stamped = corner_brightness(dir.path(), &served);
    assert!(
        stamped > base + 120.0,
        "the corner carries the mark: {base} -> {stamped}"
    );

    let (status, current) = send(
        &app,
        Request::get("/api/v2/settings/watermark")
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(current.starts_with(b"\x89PNG"));

    scratch.drop_db().await;
}
