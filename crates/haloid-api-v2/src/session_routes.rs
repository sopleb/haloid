//! Chunked uploads: open a session, send chunks in any order and as often
//! as needed, ask what has arrived, complete, or abandon.

use crate::auth_routes::body;
use crate::caller::{check_csrf, identify};
use crate::problem::Problem;
use crate::views::{FileView, UploadResponse};
use axum::body::Body;
use axum::extract::Request;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use haloid_service::app::App;
use haloid_service::chunked::{OpenSession, SessionFields, SessionState};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenSessionBody {
    pub filename: String,
    /// The whole file's size in bytes, held to this caller's limit now.
    pub size: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub album: Option<String>,
    pub nsfw: Option<bool>,
    /// Seconds until the file expires; absent or 0 for never.
    pub expires_in: Option<u64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SessionView {
    pub id: String,
    /// Every chunk but the last is exactly this long.
    pub chunk_size: u32,
    pub chunks_total: u32,
    /// Chunk numbers the server holds. The only record that counts.
    pub received: Vec<u32>,
    pub missing: Vec<u32>,
    /// `open`, `assembling`, `done`, `failed` or `expired`.
    pub status: &'static str,
    pub expires_at: DateTime<Utc>,
}

impl From<SessionState> for SessionView {
    fn from(s: SessionState) -> Self {
        Self {
            id: s.id,
            chunk_size: s.chunk_size,
            chunks_total: s.chunks_total,
            received: s.received,
            missing: s.missing,
            status: s.status.as_str(),
            expires_at: s.expires_at,
        }
    }
}

/// `POST /api/v2/uploads/sessions`.
#[utoipa::path(post, path = "/api/v2/uploads/sessions", tag = "files",
    request_body = OpenSessionBody,
    responses(
        (status = 201, description = "Opened", body = SessionView),
        (status = 403, description = "Uploads are off for this caller", body = crate::problem::ProblemBody),
        (status = 413, description = "Over this caller's size limit", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad fields, or an album that is not the caller's", body = crate::problem::ProblemBody),
    ))]
pub async fn open(
    State(app): State<Arc<App>>,
    method: Method,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, Problem> {
    let now = Utc::now();
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let request: OpenSessionBody = body(
        <Json<OpenSessionBody> as axum::extract::FromRequest<()>>::from_request(request, &()).await,
    )?;
    let state = app
        .chunked
        .open(
            caller.actor,
            OpenSession {
                filename: request.filename,
                size: request.size,
                fields: SessionFields {
                    title: request.title,
                    description: request.description,
                    tags: request.tags,
                    album: request.album,
                    nsfw: request.nsfw,
                    expires_in_secs: request.expires_in,
                },
            },
            ip,
            now,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(SessionView::from(state))).into_response())
}

/// `PUT /api/v2/uploads/sessions/{id}/chunks/{n}`, the chunk's bytes as the
/// body. Chunks count from 0; sending one again replaces it.
#[utoipa::path(put, path = "/api/v2/uploads/sessions/{id}/chunks/{n}", tag = "files",
    params(("id" = String, Path), ("n" = u32, Path)),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "Received; what the session now holds", body = SessionView),
        (status = 404, description = "No such session for this caller, or expired", body = crate::problem::ProblemBody),
        (status = 409, description = "The session is already complete", body = crate::problem::ProblemBody),
        (status = 422, description = "No such chunk, or the wrong length", body = crate::problem::ProblemBody),
    ))]
pub async fn put_chunk(
    State(app): State<Arc<App>>,
    Path((id, n)): Path<(String, u32)>,
    method: Method,
    headers: HeaderMap,
    request: Request,
) -> Result<Json<SessionView>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let stream = Body::new(request.into_body())
        .into_data_stream()
        .map_err(std::io::Error::other);
    let reader = tokio_util::io::StreamReader::new(stream);
    let state = app
        .chunked
        .put_chunk(caller.actor, &id, n, reader, now)
        .await?;
    Ok(Json(SessionView::from(state)))
}

/// `GET /api/v2/uploads/sessions/{id}`: what has arrived. Ask before
/// resuming.
#[utoipa::path(get, path = "/api/v2/uploads/sessions/{id}", tag = "files",
    params(("id" = String, Path)),
    responses(
        (status = 200, body = SessionView),
        (status = 404, description = "No such session for this caller, or expired", body = crate::problem::ProblemBody),
    ))]
pub async fn state(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SessionView>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    Ok(Json(SessionView::from(
        app.chunked.state(caller.actor, &id, now).await?,
    )))
}

/// `POST /api/v2/uploads/sessions/{id}/complete`: assemble and upload,
/// answering as `POST /api/v2/uploads` does.
#[utoipa::path(post, path = "/api/v2/uploads/sessions/{id}/complete", tag = "files",
    params(("id" = String, Path)),
    responses(
        (status = 201, description = "Stored", body = UploadResponse),
        (status = 200, description = "The same bytes were uploaded before; that file is returned", body = UploadResponse),
        (status = 404, description = "No such session for this caller, or expired", body = crate::problem::ProblemBody),
        (status = 409, description = "Already being completed", body = crate::problem::ProblemBody),
        (status = 422, description = "Chunks missing, or the file was refused", body = crate::problem::ProblemBody),
    ))]
pub async fn complete(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, Problem> {
    let now = Utc::now();
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let uploaded = app.chunked.complete(caller.actor, &id, ip, now).await?;
    let status = if uploaded.duplicate {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(UploadResponse {
            file: FileView::new(&uploaded, true, caller.actor),
            duplicate: uploaded.duplicate,
        }),
    )
        .into_response())
}

/// `DELETE /api/v2/uploads/sessions/{id}`: give up, dropping the chunks.
#[utoipa::path(delete, path = "/api/v2/uploads/sessions/{id}", tag = "files",
    params(("id" = String, Path)),
    responses(
        (status = 204, description = "Abandoned"),
        (status = 404, description = "No such session for this caller", body = crate::problem::ProblemBody),
    ))]
pub async fn abandon(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<StatusCode, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    app.chunked.abandon(caller.actor, &id, now).await?;
    Ok(StatusCode::NO_CONTENT)
}
