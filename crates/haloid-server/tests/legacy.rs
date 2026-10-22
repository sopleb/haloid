//! The legacy `/api/1/upload`, end to end over HTTP, checked three ways:
//!
//! 1. **Byte for byte against upstream** where our response must equal a
//!    real capture exactly — every error body.
//! 2. **Field order against upstream** for each captured success: the same
//!    keys in the same order, recursively. Values and typing are excluded
//!    (ids and dates differ by nature; numbers are strings by decision).
//! 3. **Golden snapshots of our own bodies**, byte for byte through `insta`
//!    with volatile values normalised but their JSON type kept, so a change
//!    of `"0"` to `0` fails here too.

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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

fn upstream(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/golden/upstream")
            .join(name),
    )
    .unwrap()
}

fn media(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/media")
        .join(name)
}

struct World {
    app: Arc<App>,
    key: String,
    dir: TempDir,
}

async fn world(db: &haloid_db::Db, settings: &[(&str, &str)]) -> World {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(
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
    );
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
    let user = haloid_db::users::create(
        db,
        &haloid_db::users::NewUser {
            username: "sharex",
            email: None,
            password_hash: None,
            role: Role::User,
            status: UserStatus::Valid,
        },
    )
    .await
    .unwrap();
    let (_, key) = app
        .auth
        .create_api_key(Some(user.id), "test")
        .await
        .unwrap();
    World { app, key, dir }
}

/// An image made on the spot, distinct per call so duplicates never fire.
fn generated(w: &World, name: &str, color: &str, size: &str) -> PathBuf {
    let path = w.dir.path().join(name);
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("color=c={color}:s={size}"))
        .args(["-frames:v", "1"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    path
}

enum Part<'a> {
    Text(&'a str, &'a str),
    File(&'a str, &'a Path),
}

/// A multipart/form-data body, field order as given.
fn multipart(parts: &[Part<'_>]) -> (String, Vec<u8>) {
    let boundary = "haloidtestboundary7d1a";
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match part {
            Part::Text(name, value) => {
                body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                        .as_bytes(),
                );
            }
            Part::File(name, path) => {
                let filename = path.file_name().unwrap().to_string_lossy();
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n\
                         Content-Type: application/octet-stream\r\n\r\n"
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(&std::fs::read(path).unwrap());
                body.extend_from_slice(b"\r\n");
            }
        }
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

struct Answer {
    status: StatusCode,
    location: Option<String>,
    content_type: String,
    body: String,
}

async fn send(w: &World, request: Request<Body>) -> Answer {
    let response = router(w.app.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v: &header::HeaderValue| v.to_str().ok())
            .map(str::to_string)
    };
    let location = header(header::LOCATION);
    let content_type = header(header::CONTENT_TYPE).unwrap_or_default();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    Answer {
        status,
        location,
        content_type,
        // Media bodies are binary; nothing compares them as text.
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn post(w: &World, path: &str, parts: &[Part<'_>]) -> Answer {
    let (content_type, body) = multipart(parts);
    send(
        w,
        Request::post(path)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

async fn get(w: &World, path: &str) -> Answer {
    send(w, Request::get(path).body(Body::empty()).unwrap()).await
}

fn payload(body: &str) -> Value {
    let parsed: Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"));
    parsed
        .get("image")
        .or_else(|| parsed.get("data"))
        .cloned()
        .unwrap_or_else(|| panic!("no payload in {body}"))
}

/// Every key path, depth first, in order. Arrays are opaque: their
/// contents' shapes are compared through the scalar fields around them.
fn key_paths(value: &Value, prefix: &str, out: &mut Vec<String>) {
    if let Value::Object(map) = value {
        for (key, item) in map {
            let path = format!("{prefix}{key}");
            out.push(path.clone());
            key_paths(item, &format!("{path}."), out);
        }
    }
}

fn shape(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    key_paths(value, "", &mut out);
    out
}

/// Replace values that differ run to run, keeping each one's JSON type.
fn normalise(value: &mut Value) {
    const VOLATILE: &[&str] = &[
        "name",
        "filename",
        "date",
        "date_gmt",
        "date_fixed_peer",
        "checksum",
        "source_checksum",
        "delete_hash",
        "delete_url",
        "id_encoded",
        "id",
        "url",
        "display_url",
        "url_viewer",
        "path_viewer",
        "url_short",
        "url_frame",
        "size",
        "thumb_size",
        "medium_size",
        "frame_size",
        "size_formatted",
        "expiration_date_gmt",
        "original_exifdata",
        "title",
        "display_title",
        "title_truncated",
        "title_truncated_html",
        "original_filename",
    ];
    if let Value::Object(map) = value {
        for (key, item) in map.iter_mut() {
            if VOLATILE.contains(&key.as_str()) {
                match item {
                    Value::String(_) => *item = Value::String(format!("<{key}>")),
                    Value::Number(_) => *item = Value::from(0),
                    _ => {}
                }
            } else {
                normalise(item);
            }
        }
    }
}

fn snapshot(name: &str, body: &str) {
    let mut parsed: Value = serde_json::from_str(body).unwrap();
    normalise(&mut parsed);
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden"));
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        insta::assert_snapshot!(name, haloid_api_legacy::php_json::to_string(&parsed));
    });
}

#[tokio::test]
async fn sharex_builtin_uploader_contract() {
    // Exactly what ShareX's built-in uploader sends: the key as a form
    // field, the file as `source`. The canary for every refactor.
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool, &[]).await;
    let file = generated(&w, "sharex.png", "0x123456", "64x48");

    let answer = post(
        &w,
        "/api/1/upload",
        &[Part::Text("key", &w.key), Part::File("source", &file)],
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(answer.content_type, "application/json; charset=UTF-8");
    let image = payload(&answer.body);
    for path in ["url", "url_viewer"] {
        assert!(
            image[path].as_str().is_some_and(|u| !u.is_empty()),
            "{path} is empty"
        );
    }
    assert!(image["image"]["url"].as_str().is_some());
    assert!(image["thumb"]["url"].as_str().is_some());

    // And the URL it was given actually serves the file.
    let url = image["url"].as_str().unwrap();
    let path = url.strip_prefix("http://localhost:8080").unwrap();
    assert_eq!(get(&w, path).await.status, StatusCode::OK);

    scratch.drop_db().await;
}

#[tokio::test]
async fn the_key_field_can_come_after_the_file() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool, &[]).await;
    let file = generated(&w, "late-key.png", "0x654321", "20x20");
    let answer = post(
        &w,
        "/api/1/upload",
        &[Part::File("source", &file), Part::Text("key", &w.key)],
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    scratch.drop_db().await;
}

#[tokio::test]
async fn error_bodies_match_upstream_byte_for_byte() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool, &[]).await;
    let svg = media("xss.svg");

    let cases: Vec<(&str, Answer, StatusCode)> = vec![
        (
            "error-no-key-json",
            get(&w, "/api/1/upload").await,
            StatusCode::BAD_REQUEST,
        ),
        (
            "error-invalid-key-json",
            get(&w, "/api/1/upload/?key=not-a-real-key").await,
            StatusCode::BAD_REQUEST,
        ),
        (
            "error-invalid-key-txt",
            get(&w, "/api/1/upload/?key=not-a-real-key&format=txt").await,
            StatusCode::OK,
        ),
        (
            "error-invalid-key-redirect",
            get(&w, "/api/1/upload/?key=not-a-real-key&format=redirect").await,
            StatusCode::OK,
        ),
        (
            "error-bad-version-json",
            get(&w, "/api/2/upload/?key=not-a-real-key").await,
            StatusCode::BAD_REQUEST,
        ),
        (
            "error-bad-action-json",
            get(&w, "/api/1/delete/?key=not-a-real-key").await,
            StatusCode::BAD_REQUEST,
        ),
        (
            "error-invalid-key-post",
            post(
                &w,
                "/api/1/upload",
                &[
                    Part::Text("key", "not-a-real-key"),
                    Part::File("source", &media("still.jpg")),
                ],
            )
            .await,
            StatusCode::BAD_REQUEST,
        ),
        (
            "error-empty-source",
            post(
                &w,
                "/api/1/upload",
                &[Part::Text("key", &w.key), Part::Text("format", "json")],
            )
            .await,
            StatusCode::BAD_REQUEST,
        ),
        (
            "error-invalid-base64",
            post(
                &w,
                "/api/1/upload",
                &[
                    Part::Text("key", &w.key),
                    Part::Text("source", "not base64 at all!!"),
                ],
            )
            .await,
            StatusCode::BAD_REQUEST,
        ),
        (
            "error-unsupported-type",
            post(
                &w,
                "/api/1/upload",
                &[Part::Text("key", &w.key), Part::File("source", &svg)],
            )
            .await,
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (name, answer, status) in cases {
        assert_eq!(answer.status, status, "{name}");
        assert_eq!(answer.body, upstream(&format!("{name}.body")), "{name}");
    }

    let header_key = send(
        &w,
        Request::get("/api/1/upload")
            .header("x-api-key", "not-a-real-key")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(header_key.body, upstream("error-header-key.body"));

    scratch.drop_db().await;
}

#[tokio::test]
async fn success_bodies_have_upstreams_field_order() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool, &[]).await;

    let small = generated(&w, "small.jpg", "0x336699", "40x20");
    let large = generated(&w, "large.jpg", "0x993366", "900x600");
    let width = generated(&w, "width.jpg", "0x666633", "300x200");
    let params = generated(&w, "params.jpg", "0x336666", "50x25");
    let b64 = generated(&w, "b64.png", "0x663366", "20x20");
    let imgbb = generated(&w, "imgbb.png", "0x339966", "30x30");
    use base64::Engine;
    let encode =
        |p: &Path| base64::engine::general_purpose::STANDARD.encode(std::fs::read(p).unwrap());

    let k = w.key.as_str();
    let b64_text = encode(&b64);
    let imgbb_text = encode(&imgbb);
    let cases: Vec<(&str, Vec<Part<'_>>)> = vec![
        (
            "success-small-jpeg",
            vec![Part::Text("key", k), Part::File("source", &small)],
        ),
        (
            "success-large-jpeg",
            vec![Part::Text("key", k), Part::File("source", &large)],
        ),
        (
            "success-width",
            vec![
                Part::Text("key", k),
                Part::File("source", &width),
                Part::Text("width", "120"),
            ],
        ),
        (
            "success-params",
            vec![
                Part::Text("key", k),
                Part::File("source", &params),
                Part::Text("title", "Golden title"),
                Part::Text("description", "Golden description"),
                Part::Text("tags", "golden,fixture"),
                Part::Text("nsfw", "1"),
                Part::Text("expiration", "PT1H"),
            ],
        ),
        (
            "success-base64",
            vec![Part::Text("key", k), Part::Text("source", &b64_text)],
        ),
        (
            "success-imgbb-envelope",
            vec![Part::Text("key", k), Part::Text("image", &imgbb_text)],
        ),
    ];

    for (name, parts) in cases {
        let answer = post(&w, "/api/1/upload", &parts).await;
        assert_eq!(answer.status, StatusCode::OK, "{name}: {}", answer.body);

        let ours: Value = serde_json::from_str(&answer.body).unwrap();
        let theirs: Value = serde_json::from_str(&upstream(&format!("{name}.body"))).unwrap();
        assert_eq!(
            ours.as_object().unwrap().keys().collect::<Vec<_>>(),
            theirs.as_object().unwrap().keys().collect::<Vec<_>>(),
            "{name}: envelope"
        );
        assert_eq!(
            shape(&payload(&answer.body)),
            shape(&payload(&upstream(&format!("{name}.body")))),
            "{name}: field order"
        );

        snapshot(name, &answer.body);
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn numbers_are_strings_and_values_follow_upstreams_rules() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool, &[]).await;
    let large = generated(&w, "values.jpg", "0x445566", "900x600");
    let answer = post(
        &w,
        "/api/1/upload",
        &[Part::Text("key", &w.key), Part::File("source", &large)],
    )
    .await;
    let image = payload(&answer.body);

    for field in [
        "size",
        "width",
        "height",
        "views",
        "likes",
        "nsfw",
        "is_animated",
        "is_approved",
        "is_360",
        "duration",
    ] {
        assert!(
            image[field].is_string(),
            "{field} is not a string: {}",
            image[field]
        );
    }
    assert_eq!(image["width"], "900");
    assert_eq!(image["chain"], 7, "chain stays an integer");
    assert_eq!(image["ratio"], 1.5);
    assert_eq!(
        image["display_width"], "500",
        "from the setting, so a string"
    );
    assert_eq!(image["display_height"], 333, "computed, so an integer");
    assert_eq!(image["source_checksum"], Value::Null, "unaltered upload");
    assert_eq!(image["title"], "values", "the file name's stem");
    assert_eq!(image["file"]["resource"]["type"], "path");
    assert!(
        answer.body.contains(r"https:\/\/") || answer.body.contains(r"http:\/\/"),
        "PHP escaping"
    );
    assert!(
        image["delete_hash"]
            .as_str()
            .is_some_and(|h| h.starts_with("$argon2id$"))
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn text_and_redirect_formats_answer_as_upstream_does() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool, &[]).await;
    let txt = generated(&w, "txt.jpg", "0x111111", "8x8");
    let redirect = generated(&w, "redirect.jpg", "0x222222", "8x8");

    let answer = post(
        &w,
        "/api/1/upload",
        &[
            Part::Text("key", &w.key),
            Part::File("source", &txt),
            Part::Text("format", "txt"),
        ],
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.starts_with("http://localhost:8080/files/"),
        "{}",
        answer.body
    );
    assert!(answer.body.ends_with(".jpg"));

    let answer = post(
        &w,
        "/api/1/upload",
        &[
            Part::Text("key", &w.key),
            Part::File("source", &redirect),
            Part::Text("format", "redirect"),
        ],
    )
    .await;
    assert_eq!(answer.status, StatusCode::FOUND, "upstream answers 302");
    let location = answer.location.expect("a Location");
    assert!(location.starts_with("/f/"), "{location}");

    scratch.drop_db().await;
}

#[tokio::test]
async fn base64_by_get_is_refused_with_upstreams_message() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool, &[]).await;
    let answer = get(
        &w,
        &format!("/api/1/upload/?key={}&source=aGVsbG8%3D", w.key),
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        payload_error(&answer.body),
        (
            "Upload using base64 source must be done using POST method.".to_string(),
            130
        )
    );
    scratch.drop_db().await;
}

fn payload_error(body: &str) -> (String, i64) {
    let parsed: Value = serde_json::from_str(body).unwrap();
    (
        parsed["error"]["message"].as_str().unwrap().to_string(),
        parsed["error"]["code"].as_i64().unwrap(),
    )
}

#[tokio::test]
async fn an_unapproved_upload_loses_its_urls_except_the_frame() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool, &[("moderation.mode", "all")]).await;
    let answer = post(
        &w,
        "/api/1/upload",
        &[
            Part::Text("key", &w.key),
            Part::File("source", &media("clip.mp4")),
        ],
    )
    .await;
    assert_eq!(
        answer.status,
        StatusCode::OK,
        "still a 200: {}",
        answer.body
    );
    let image = payload(&answer.body);

    assert_eq!(image["is_approved"], "0");
    for removed in ["url", "display_url"] {
        assert!(image.get(removed).is_none(), "{removed} is present");
    }
    for derivative in ["image", "thumb"] {
        assert!(
            image[derivative].get("url").is_none(),
            "{derivative}.url is present"
        );
        assert!(image[derivative].get("filename").is_some());
    }
    assert!(image["medium"].get("url").is_none());
    assert!(
        image["frame"]["url"].as_str().is_some(),
        "frame.url survives"
    );
    assert!(image["url_frame"].as_str().is_some_and(|u| !u.is_empty()));

    // ...and names a file nobody can fetch until a moderator approves.
    let frame = image["frame"]["url"]
        .as_str()
        .unwrap()
        .strip_prefix("http://localhost:8080")
        .unwrap()
        .to_string();
    assert_eq!(get(&w, &frame).await.status, StatusCode::NOT_FOUND);

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_video_carries_its_frame_and_duration() {
    let scratch = migrated_or_skip!();
    let w = world(&scratch.pool, &[]).await;
    let answer = post(
        &w,
        "/api/1/upload",
        &[
            Part::Text("key", &w.key),
            Part::File("source", &media("clip.mp4")),
        ],
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let image = payload(&answer.body);
    assert_eq!(image["type"], "video");
    assert_eq!(image["chain"], 21);
    assert_eq!(image["duration"], "1");
    assert_eq!(image["duration_time"], "00:01");
    let keys = shape(&image);
    let position = |k: &str| keys.iter().position(|p| p == k).unwrap();
    assert!(position("frame") < position("image") && position("image") < position("thumb"));
    assert_eq!(image["display_url"], image["frame"]["url"]);
    snapshot("success-video", &answer.body);

    scratch.drop_db().await;
}
