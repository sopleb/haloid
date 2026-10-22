//! The `/api/{version}/{action}` route.
//!
//! Upstream answers every path under `/api/`, so that a wrong version or
//! action is an error in its own envelope rather than a 404; so do we, with
//! and without a trailing slash. (Upstream also 301-redirects a request with
//! a query string to the trailing-slash form; we do not, because a client
//! following a redirect may resend its POST as a GET.)
//!
//! A multipart body's file can arrive before its key, and nothing may be
//! uploaded before the key is checked. So the file is buffered to a
//! temporary file first — capped at the largest limit any role has — and
//! only handed to the pipeline once the request is authorised. The pipeline
//! then applies the caller's own limit.

use crate::errors::describe;
use crate::request::{self, Format, LegacyRequest, RawRequest, Source};
use crate::response::{self, Context};
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, FromRequest, Multipart, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use chrono::Utc;
use haloid_core::DomainError;
use haloid_core::auth::{Actor, Role};
use haloid_core::expiration::parse_expiration;
use haloid_service::app::App;
use haloid_service::upload::{UploadCommand, UploadSource};
use rand::Rng;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/{version}/{action}", any(handle))
        .route("/api/{version}/{action}/", any(handle))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(app)
}

/// A buffered upload, removed when dropped.
struct Buffered(PathBuf);

impl Drop for Buffered {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct Received {
    raw: RawRequest,
    file: Option<Buffered>,
    filename: Option<String>,
}

async fn handle(
    State(app): State<Arc<App>>,
    Path((version, action)): Path<(String, String)>,
    request: Request,
) -> Response {
    // Present when served with connect info; absent in in-process tests.
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let format_hint = request.uri().query().map(query_pairs).unwrap_or_default();
    let early_format = request::format(&RawRequest {
        query: format_hint,
        ..RawRequest::default()
    });

    let received = match receive(&app, version, action, request).await {
        Ok(received) => received,
        Err(error) => return render_error(&error, early_format),
    };
    let format = request::format(&received.raw);
    match run(&app, received, ip).await {
        Ok((body, imgbb_url, viewer)) => render_success(format, body, imgbb_url, viewer),
        Err(error) => render_error(&error, format),
    }
}

fn query_pairs(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

/// Read the request: query string, body fields, and the `source` file.
async fn receive(
    app: &App,
    version: String,
    action: String,
    request: Request,
) -> Result<Received, DomainError> {
    let settings = app.settings.get();
    // The largest any role may send; the pipeline enforces the caller's own.
    let cap = settings.max_upload_size(Actor::User {
        id: haloid_core::ids::UserId(0),
        role: Role::Admin,
    });
    let is_get = request.method() == Method::GET;
    let header_key = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let query = request.uri().query().map(query_pairs).unwrap_or_default();
    let content_type = content_type(request.headers());

    let mut raw = RawRequest {
        version,
        action,
        is_get,
        header_key,
        query,
        body: Vec::new(),
        has_source_file: false,
    };
    let mut file = None;
    let mut filename = None;

    if content_type.starts_with("multipart/form-data") {
        let mut multipart = Multipart::from_request(request, &())
            .await
            .map_err(|_| DomainError::EmptySource)?;
        while let Some(mut field) = multipart
            .next_field()
            .await
            .map_err(|_| DomainError::EmptySource)?
        {
            let name = field.name().unwrap_or_default().to_string();
            if field.file_name().is_some() {
                // Only `source` is read as a file, as upstream does.
                if name == "source" && file.is_none() {
                    filename = field.file_name().map(str::to_string);
                    let path = app
                        .uploads
                        .temp_dir()
                        .join(format!("legacy-{:016x}", rand::rng().random::<u64>()));
                    tokio::fs::create_dir_all(app.uploads.temp_dir())
                        .await
                        .map_err(internal)?;
                    let buffered = Buffered(path);
                    let mut out = tokio::fs::File::create(&buffered.0)
                        .await
                        .map_err(internal)?;
                    let mut total: u64 = 0;
                    while let Some(chunk) =
                        field.chunk().await.map_err(|_| DomainError::EmptySource)?
                    {
                        total += chunk.len() as u64;
                        if total > cap {
                            return Err(DomainError::SizeLimit {
                                size: total,
                                limit: cap,
                            });
                        }
                        out.write_all(&chunk).await.map_err(internal)?;
                    }
                    out.flush().await.map_err(internal)?;
                    raw.has_source_file = total > 0;
                    file = Some(buffered);
                } else {
                    while field.chunk().await.ok().flatten().is_some() {}
                }
            } else {
                // A text field may be a base64 source, so it can be large;
                // never larger than the biggest upload as base64.
                let text_cap = cap.saturating_mul(4) / 3 + 4096;
                let mut text = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(|_| DomainError::EmptySource)? {
                    text.extend_from_slice(&chunk);
                    if text.len() as u64 > text_cap {
                        return Err(DomainError::SizeLimit {
                            size: text.len() as u64,
                            limit: cap,
                        });
                    }
                }
                raw.body
                    .push((name, String::from_utf8_lossy(&text).into_owned()));
            }
        }
    } else if content_type.starts_with("application/x-www-form-urlencoded") {
        let limit = usize::try_from(cap.saturating_mul(4) / 3 + 65_536).unwrap_or(usize::MAX);
        let bytes = axum::body::to_bytes(request.into_body(), limit)
            .await
            .map_err(|_| DomainError::SizeLimit {
                size: cap.saturating_add(1),
                limit: cap,
            })?;
        raw.body = url::form_urlencoded::parse(&bytes).into_owned().collect();
    }

    Ok(Received {
        raw,
        file,
        filename,
    })
}

fn content_type(headers: &HeaderMap) -> String {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn internal(error: std::io::Error) -> DomainError {
    DomainError::Internal(Box::new(error))
}

/// Authorise and upload, in upstream's order of checks: the request's
/// shape, the key, permission to upload, the action, the source.
async fn run(
    app: &App,
    received: Received,
    ip: Option<std::net::IpAddr>,
) -> Result<(serde_json::Value, Option<String>, String), DomainError> {
    let now = Utc::now();
    let parsed: LegacyRequest = request::parse(&received.raw)?;
    let actor = app.auth.api_key_actor(&parsed.key, now).await?;
    let settings = app.settings.get();
    if !settings.may_upload(actor) {
        return Err(DomainError::Forbidden);
    }
    parsed.check_action()?;

    let source = match parsed.source()? {
        Source::File => {
            let buffered = received.file.as_ref().ok_or(DomainError::EmptySource)?;
            let reader = tokio::fs::File::open(&buffered.0).await.map_err(internal)?;
            UploadSource::Stream {
                reader: Box::new(reader),
                filename: received.filename.clone(),
            }
        }
        Source::Base64(data) => UploadSource::Base64 {
            data,
            // Upstream names a base64 upload with twelve random hex
            // characters, which then become its title.
            filename: Some(hex_name()),
        },
        Source::Url(url) => UploadSource::Url(url),
    };
    let expiration = match parsed.expiration.as_deref() {
        Some(raw) => parse_expiration(raw, settings.uploads.max_expiration)?,
        None => None,
    };

    let uploaded = app
        .uploads
        .upload(UploadCommand {
            actor,
            source,
            title: parsed.title.clone(),
            description: parsed.description.clone(),
            tags: parsed.tags.clone(),
            album: parsed.album_id.clone(),
            category: parsed.category_id.map(haloid_core::ids::CategoryId),
            nsfw: parsed.nsfw,
            expiration,
            width: parsed.width,
            uploader_ip: ip,
        })
        .await?;
    // The buffered copy is no longer needed once the pipeline has its own.
    drop(received.file);

    let ctx = Context {
        base_url: &settings.site.base_url,
        thumb_edge: settings.derivatives.thumb_edge,
        medium_edge: settings.derivatives.medium_edge,
        medium_dimension: settings.derivatives.medium_fixed_dimension,
        now,
    };
    let image = response::image_object(&uploaded, &ctx);
    let url = image
        .get("url")
        .and_then(|u| u.as_str())
        .map(str::to_string);
    let viewer = image
        .get("path_viewer")
        .and_then(|p| p.as_str())
        .unwrap_or("/")
        .to_string();
    Ok((response::success(image, parsed.imgbb), url, viewer))
}

fn hex_name() -> String {
    hex::encode(rand::rng().random::<[u8; 6]>())
}

fn render_success(
    format: Format,
    body: serde_json::Value,
    url: Option<String>,
    viewer: String,
) -> Response {
    match format {
        Format::Json => json(StatusCode::OK, &body),
        // An unapproved file has no URL; upstream echoes nothing then.
        Format::Txt => text(url.unwrap_or_default()),
        Format::Redirect => {
            let mut response = StatusCode::FOUND.into_response();
            if let Ok(location) = HeaderValue::from_str(&viewer) {
                response.headers_mut().insert(header::LOCATION, location);
            }
            response
        }
    }
}

fn render_error(error: &DomainError, format: Format) -> Response {
    if !error.is_client_safe() {
        tracing::error!(%error, source = ?std::error::Error::source(error), "legacy upload failed");
    }
    let (message, code) = describe(error);
    match format {
        Format::Json => json(StatusCode::BAD_REQUEST, &response::error(&message, code)),
        // Both text formats answer an error with HTTP 200 and the bare
        // message: upstream's contract, captured.
        Format::Txt | Format::Redirect => text(message),
    }
}

fn json(status: StatusCode, body: &serde_json::Value) -> Response {
    let mut response = (status, crate::php_json::to_string(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    response
}

fn text(body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=UTF-8"),
    );
    response
}
