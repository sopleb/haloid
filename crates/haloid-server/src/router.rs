//! The HTTP router: health checks, both APIs, the media files, and the
//! single-page app — from a directory on disk, or compiled into the binary
//! with the `embed-frontend` feature.

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use haloid_service::app::App;
use std::sync::Arc;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

/// The router without a frontend, as the API tests use it.
pub fn router(app: Arc<App>) -> Router {
    app_router(app, None)
}

/// Where the single-page app comes from.
#[derive(Debug, Clone, Copy)]
pub enum Frontend<'a> {
    /// None: the API alone.
    None,
    /// A built `frontend/build` on disk.
    Dir(&'a std::path::Path),
    /// The copy compiled into this binary.
    Embedded,
}

/// The copy of `frontend/build` compiled in, if this binary has one.
#[cfg(feature = "embed-frontend")]
pub mod embedded {
    #[derive(rust_embed::Embed)]
    #[folder = "../../frontend/build/"]
    pub struct Assets;

    pub fn get(path: &str) -> Option<(std::borrow::Cow<'static, [u8]>, String)> {
        Assets::get(path).map(|f| (f.data, f.metadata.mimetype().to_string()))
    }
}

/// Whether this binary carries the frontend.
pub const fn has_embedded_frontend() -> bool {
    cfg!(feature = "embed-frontend")
}

/// The full router: the API, media, and — when built — the single-page app
/// from `frontend`, with every unknown non-API path falling back to its
/// `index.html` so the browser's router can take over.
pub fn app_router(app: Arc<App>, frontend: Option<&std::path::Path>) -> Router {
    app_router_with(app, frontend.map_or(Frontend::None, Frontend::Dir))
}

pub fn app_router_with(app: Arc<App>, frontend: Frontend<'_>) -> Router {
    let mut router = Router::new()
        .route("/api/v2/healthz", get(healthz))
        .route("/api/v2/readyz", get(readyz))
        // An imported Chevereto's links: `/image/<slug>.<id>` and
        // `/album/<slug>.<id>` (or the id alone) go to the same thing here.
        .route("/image/{tail}", get(legacy_image))
        .route("/album/{tail}", get(legacy_album))
        .with_state(app.clone())
        .merge(haloid_api_legacy::router(app.clone()))
        .merge(haloid_api_v2::router(app.clone()));

    for served in app.storages.served_roots() {
        if mount_path(&served.url_prefix).is_none() {
            tracing::warn!(
                url_prefix = %served.url_prefix,
                "a local storage's URL prefix has no path to mount; serve it from elsewhere"
            );
        }
    }
    match frontend {
        Frontend::None => {}
        Frontend::Dir(dir) => {
            router = router
                .merge(public_pages(
                    app.clone(),
                    Index::Dir(dir.join("index.html")),
                ))
                .fallback_service(spa(dir));
        }
        Frontend::Embedded => {
            // The middleware wraps the fallback alone: on the whole router
            // it would turn every API route into a 404.
            let assets = Router::new()
                .fallback(embedded_asset)
                .layer(middleware::from_fn(api_paths_are_not_pages))
                .layer(middleware::from_fn(cache_policy));
            router = router
                .merge(public_pages(app.clone(), Index::Embedded))
                .fallback_service(assets);
        }
    }
    // Local storages are looked up per request, not mounted once, so one
    // added or changed in the admin area is served at once.
    let pool = app.clone();
    router = router.layer(middleware::from_fn(move |request: Request, next: Next| {
        let app = pool.clone();
        async move { local_media(&app, request, next).await }
    }));
    let counted = app.metrics.clone();
    router
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let metrics = counted.clone();
                async move {
                    let response = next.run(request).await;
                    metrics.response(response.status().as_u16());
                    response
                }
            },
        ))
        .layer(TraceLayer::new_for_http())
}

#[derive(Clone)]
enum Index {
    Dir(std::path::PathBuf),
    Embedded,
}

impl Index {
    async fn read(&self) -> Option<String> {
        match self {
            Self::Dir(path) => tokio::fs::read_to_string(path).await.ok(),
            Self::Embedded => embedded_file("index.html")
                .and_then(|(data, _)| String::from_utf8(data.into_owned()).ok()),
        }
    }
}

#[cfg(feature = "embed-frontend")]
fn embedded_file(path: &str) -> Option<(std::borrow::Cow<'static, [u8]>, String)> {
    embedded::get(path)
}

#[cfg(not(feature = "embed-frontend"))]
fn embedded_file(_: &str) -> Option<(std::borrow::Cow<'static, [u8]>, String)> {
    None
}

/// The embedded SPA: a file if there is one by that path, else
/// `index.html` for the browser's router.
async fn embedded_asset(request: Request) -> Response {
    let path = request.uri().path().trim_start_matches('/');
    let found = (!path.is_empty())
        .then(|| embedded_file(path))
        .flatten()
        .or_else(|| embedded_file("index.html"));
    match found {
        Some((data, mime)) => ([(header::CONTENT_TYPE, mime)], data.into_owned()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Clone)]
struct Pages {
    app: Arc<App>,
    index: Index,
}

/// The public pages, served as `index.html` with link-preview tags for
/// whatever a guest may see there (see `head`).
fn public_pages(app: Arc<App>, index: Index) -> Router {
    let state = Pages { app, index };
    Router::new()
        .route("/f/{id}", get(public_page))
        .route("/f/{id}/{slug}", get(public_page))
        .route("/a/{id}", get(public_page))
        .route("/a/{id}/{slug}", get(public_page))
        .route("/u/{username}", get(public_page))
        .with_state(state)
}

async fn public_page(State(pages): State<Pages>, request: Request) -> Response {
    let Some(index) = pages.index.read().await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let tags = crate::head::for_path(&pages.app, request.uri().path())
        .await
        .unwrap_or_default();
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        crate::head::inject(&index, &tags),
    )
        .into_response()
}

/// The built SPA. Hashed assets under `_app/immutable/` are cached for a
/// year; everything else, `index.html` above all, is revalidated so a
/// deploy shows up at once.
fn spa(dir: &std::path::Path) -> Router {
    let index = dir.join("index.html");
    Router::new()
        .fallback_service(
            ServeDir::new(dir)
                .append_index_html_on_directories(false)
                .fallback(tower_http::services::ServeFile::new(index)),
        )
        .layer(middleware::from_fn(api_paths_are_not_pages))
        .layer(middleware::from_fn(cache_policy))
}

/// An unknown `/api/…` path is a 404, never the SPA's HTML: an API client
/// that mistypes a path should get an error it can read.
async fn api_paths_are_not_pages(request: Request, next: Next) -> Response {
    if request.uri().path().starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/problem+json")],
            r#"{"type":"about:blank","title":"Not Found","status":404,"detail":"no such endpoint","code":"not_found"}"#,
        )
            .into_response();
    }
    next.run(request).await
}

async fn cache_policy(request: Request, next: Next) -> Response {
    let immutable = request.uri().path().starts_with("/_app/immutable/");
    let mut response = next.run(request).await;
    let value = if immutable {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    response
}

/// Liveness: the process answers. Depends on nothing.
async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Readiness: the database answers too.
async fn readyz(State(app): State<Arc<App>>) -> Response {
    match haloid_db::ping(&app.db).await {
        Ok(()) => Json(serde_json::json!({ "status": "ready" })).into_response(),
        Err(error) => {
            tracing::warn!(%error, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "status": "unavailable" })),
            )
                .into_response()
        }
    }
}

/// The path part of a storage's URL prefix, where its directory is
/// mounted: `https://img.example.com/files` → `/files`. `None` for a prefix
/// with no path — a storage served from its own domain by something else.
pub fn mount_path(url_prefix: &str) -> Option<String> {
    let after_scheme = url_prefix
        .split_once("://")
        .map_or(url_prefix, |(_, rest)| rest);
    let path = after_scheme.find('/').map(|i| &after_scheme[i..])?;
    let path = path.trim_end_matches('/');
    (!path.is_empty()).then(|| path.to_string())
}

/// A storage's served directory as static files.
///
/// Only the directory itself: its quarantine is a separate tree that is
/// never mounted. Dot-files — the half-written temporaries beside an object
/// being stored — are hidden. `nosniff` stops a browser second-guessing a
/// type the pipeline already verified, and the cache lifetime is kept short
/// because a deleted file must stop being served soon after it is deleted.
fn media(root: &std::path::Path) -> Router {
    Router::new()
        .fallback_service(ServeDir::new(root).append_index_html_on_directories(false))
        .layer(middleware::from_fn(hide_dotfiles))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=3600"),
        ))
}

/// Serve a request from the local storage whose URL prefix it falls under,
/// the longest prefix winning; anything else goes on to the router. The API
/// is never shadowed, whatever an administrator sets as a prefix.
async fn local_media(app: &App, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    if path.starts_with("/api/") {
        return next.run(request).await;
    }
    let found = app
        .storages
        .served_roots()
        .into_iter()
        .filter_map(|served| mount_path(&served.url_prefix).map(|mount| (mount, served.root)))
        .filter(|(mount, _)| path == *mount || path.starts_with(&format!("{mount}/")))
        .max_by_key(|(mount, _)| mount.len());
    let Some((mount, root)) = found else {
        return next.run(request).await;
    };
    let (mut parts, body) = request.into_parts();
    let rest = match &path[mount.len()..] {
        "" => "/",
        rest => rest,
    };
    let rewritten = match parts.uri.query() {
        Some(query) => format!("{rest}?{query}"),
        None => rest.to_string(),
    };
    match rewritten.parse() {
        Ok(uri) => parts.uri = uri,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    }
    use tower::ServiceExt;
    match media(&root).oneshot(Request::from_parts(parts, body)).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// The old id at the end of a Chevereto page path: after the last dot, or
/// the whole of it.
fn legacy_id(tail: &str) -> &str {
    tail.rsplit('.').next().unwrap_or(tail)
}

async fn legacy_redirect(
    app: &App,
    kind: haloid_service::library::LegacyKind,
    tail: &str,
) -> Response {
    match app.library.legacy_link(kind, legacy_id(tail)).await {
        Ok(Some(id)) => {
            let to = match kind {
                haloid_service::library::LegacyKind::File => format!("/f/{id}"),
                haloid_service::library::LegacyKind::Album => format!("/a/{id}"),
            };
            axum::response::Redirect::permanent(&to).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(%error, "looking up an old link failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn legacy_image(
    axum::extract::State(app): axum::extract::State<Arc<App>>,
    axum::extract::Path(tail): axum::extract::Path<String>,
) -> Response {
    legacy_redirect(&app, haloid_service::library::LegacyKind::File, &tail).await
}

async fn legacy_album(
    axum::extract::State(app): axum::extract::State<Arc<App>>,
    axum::extract::Path(tail): axum::extract::Path<String>,
) -> Response {
    legacy_redirect(&app, haloid_service::library::LegacyKind::Album, &tail).await
}

async fn hide_dotfiles(request: Request, next: Next) -> Response {
    let hidden = request
        .uri()
        .path()
        .split('/')
        .any(|segment| segment.starts_with('.'));
    if hidden {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::{legacy_id, mount_path};

    #[test]
    fn an_old_page_path_ends_in_its_id() {
        assert_eq!(legacy_id("sunset-at-the-bay.WNN"), "WNN");
        assert_eq!(legacy_id("WNN"), "WNN");
        assert_eq!(legacy_id("v1.2.final.Ab9"), "Ab9");
    }

    #[test]
    fn mount_paths_come_from_the_prefix() {
        assert_eq!(
            mount_path("https://img.example.com/files").as_deref(),
            Some("/files")
        );
        assert_eq!(
            mount_path("http://localhost:8080/files/").as_deref(),
            Some("/files")
        );
        assert_eq!(mount_path("https://x.com/a/b").as_deref(), Some("/a/b"));
        assert_eq!(mount_path("https://cdn.example.com"), None);
        assert_eq!(mount_path("https://cdn.example.com/"), None);
    }
}
