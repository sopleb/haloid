//! The v2 router, and the upload and read-a-file handlers. Library reads,
//! auth and writes live in their own modules.

use crate::caller::{album_grants, check_csrf, identify};
use crate::problem::Problem;
use crate::views::{FileView, UploadResponse};
use axum::extract::{ConnectInfo, Multipart, Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Router, extract::Request};
use chrono::Utc;
use haloid_core::DomainError;
use haloid_service::app::App;
use haloid_service::upload::{UploadCommand, UploadSource};
use rand::Rng;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/v2/uploads", post(upload))
        .route(
            "/api/v2/uploads/sessions",
            post(crate::session_routes::open),
        )
        .route(
            "/api/v2/uploads/sessions/{id}",
            get(crate::session_routes::state).delete(crate::session_routes::abandon),
        )
        .route(
            "/api/v2/uploads/sessions/{id}/chunks/{n}",
            axum::routing::put(crate::session_routes::put_chunk),
        )
        .route(
            "/api/v2/uploads/sessions/{id}/complete",
            post(crate::session_routes::complete),
        )
        .route("/api/v2/files", get(crate::library_routes::explore))
        .route("/api/v2/files/bulk", post(crate::write_routes::bulk))
        .route(
            "/api/v2/files/{id}",
            get(file)
                .patch(crate::write_routes::edit_file)
                .delete(crate::write_routes::delete_file),
        )
        .route("/api/v2/files/{id}/view", post(view))
        .route("/api/v2/albums", post(crate::write_routes::create_album))
        .route("/api/v2/users/{username}", get(crate::library_routes::user))
        .route(
            "/api/v2/users/{username}/files",
            get(crate::library_routes::user_files),
        )
        .route(
            "/api/v2/users/{username}/albums",
            get(crate::library_routes::user_albums),
        )
        .route(
            "/api/v2/albums/{id}",
            get(crate::library_routes::album)
                .patch(crate::write_routes::edit_album)
                .delete(crate::write_routes::delete_album),
        )
        .route(
            "/api/v2/albums/{id}/order",
            axum::routing::put(crate::write_routes::reorder_album),
        )
        .route(
            "/api/v2/albums/{id}/files",
            get(crate::library_routes::album_files),
        )
        .route("/api/v2/config", get(crate::library_routes::config))
        .route(
            "/api/v2/storages",
            get(crate::admin_routes::storages).post(crate::admin_routes::create_storage),
        )
        .route(
            "/api/v2/storages/{id}",
            axum::routing::patch(crate::admin_routes::update_storage),
        )
        .route(
            "/api/v2/storages/{id}/test",
            post(crate::admin_routes::test_storage),
        )
        .route(
            "/api/v2/storages/{id}/retire",
            post(crate::admin_routes::retire_storage),
        )
        .route(
            "/api/v2/moderation/queue",
            get(crate::admin_routes::moderation_queue),
        )
        .route(
            "/api/v2/moderation/{id}/approve",
            post(crate::admin_routes::approve),
        )
        .route(
            "/api/v2/moderation/{id}/deny",
            post(crate::admin_routes::deny),
        )
        .route(
            "/api/v2/settings",
            get(crate::admin_routes::settings).patch(crate::admin_routes::update_settings),
        )
        .route("/api/v2/updates", get(crate::admin_routes::updates))
        .route(
            "/api/v2/updates/check",
            post(crate::admin_routes::check_updates),
        )
        .route(
            "/api/v2/updates/install",
            post(crate::admin_routes::install_update),
        )
        .route(
            "/api/v2/licence",
            get(crate::admin_routes::licence)
                .put(crate::admin_routes::install_licence)
                .delete(crate::admin_routes::remove_licence),
        )
        .route(
            "/api/v2/settings/watermark",
            get(crate::admin_routes::watermark)
                .put(crate::admin_routes::set_watermark)
                .delete(crate::admin_routes::remove_watermark),
        )
        .route("/api/v2/admin/users", get(crate::admin_routes::users))
        .route(
            "/api/v2/admin/users/{id}",
            axum::routing::patch(crate::admin_routes::update_user),
        )
        .route(
            "/api/v2/admin/files/deleted",
            get(crate::admin_routes::deleted_files),
        )
        .route(
            "/api/v2/admin/files/{id}/restore",
            post(crate::admin_routes::restore_file),
        )
        .route(
            "/api/v2/admin/files/{id}/purge",
            post(crate::admin_routes::purge_file),
        )
        .route("/api/v2/admin/audit", get(crate::admin_routes::audit_log))
        .route("/api/v2/metrics", get(crate::admin_routes::metrics))
        .route("/api/v2/openapi.json", get(openapi_json))
        .route("/api/v2/auth/challenge", get(crate::auth_routes::challenge))
        .route("/api/v2/auth/register", post(crate::auth_routes::register))
        .route("/api/v2/auth/login", post(crate::auth_routes::login))
        .route("/api/v2/auth/logout", post(crate::auth_routes::logout))
        .route(
            "/api/v2/auth/me",
            get(crate::auth_routes::me).patch(crate::account_routes::update_me),
        )
        .route(
            "/api/v2/auth/password",
            post(crate::account_routes::change_password),
        )
        .route(
            "/api/v2/auth/api-keys",
            get(crate::account_routes::api_keys).post(crate::account_routes::create_api_key),
        )
        .route(
            "/api/v2/auth/api-keys/{id}",
            axum::routing::delete(crate::account_routes::revoke_api_key),
        )
        .route(
            "/api/v2/auth/sessions",
            get(crate::account_routes::sessions),
        )
        .route(
            "/api/v2/auth/sessions/{id}",
            axum::routing::delete(crate::account_routes::revoke_session),
        )
        .route(
            "/api/v2/albums/{id}/unlock",
            post(crate::account_routes::unlock_album),
        )
        // Everything else under /api/v2 is ours to answer: without this the
        // legacy surface's /api/{version}/{action} would claim it and reply
        // in the legacy envelope.
        .route("/api/v2/{*rest}", axum::routing::any(no_such_endpoint))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(app)
}

/// A request body's file held on disk until the pipeline takes it, removed
/// when dropped.
struct Buffered(PathBuf);

impl Drop for Buffered {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn internal(error: std::io::Error) -> DomainError {
    DomainError::Internal(Box::new(error))
}

/// `POST /api/v2/uploads`, multipart: a `file` (or a `url` to fetch it
/// from, where the instance allows that), and optionally `title`,
/// `description`, `tags` (comma-separated or repeated), `album`, `nsfw`,
/// `expires_in` (seconds) and `width`.
#[utoipa::path(post, path = "/api/v2/uploads", tag = "files",
    request_body(content = crate::openapi::UploadForm, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Stored", body = UploadResponse),
        (status = 200, description = "The same bytes were uploaded before; that file is returned", body = UploadResponse),
        (status = 401, description = "A bearer key that is wrong or revoked", body = crate::problem::ProblemBody),
        (status = 403, description = "Uploads are off for this caller, or a cookie request not from our page", body = crate::problem::ProblemBody),
        (status = 413, description = "Over this caller's size or pixel limit", body = crate::problem::ProblemBody),
        (status = 422, description = "Not an accepted type, unreadable, or bad fields", body = crate::problem::ProblemBody),
    ))]
async fn upload(
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
    let settings = app.settings.get();
    check_csrf(&caller, &method, &headers, &settings.site.base_url)?;
    if !settings.may_upload(caller.actor) {
        return Err(DomainError::Forbidden.into());
    }
    let limit = settings.max_upload_size(caller.actor);

    let mut multipart = <Multipart as axum::extract::FromRequest<()>>::from_request(request, &())
        .await
        .map_err(|_| DomainError::invalid_param("body", "expected multipart/form-data"))?;

    let mut buffered: Option<(Buffered, Option<String>)> = None;
    let mut title = None;
    let mut description = None;
    let mut tags = Vec::new();
    let mut album = None;
    let mut nsfw = None;
    let mut expires_in = None;
    let mut width = None;
    let mut url = None;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| DomainError::invalid_param("body", "malformed multipart body"))?
    {
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" && buffered.is_none() {
            let filename = field.file_name().map(str::to_string);
            tokio::fs::create_dir_all(app.uploads.temp_dir())
                .await
                .map_err(internal)?;
            let held = Buffered(
                app.uploads
                    .temp_dir()
                    .join(format!("v2-{:016x}", rand::rng().random::<u64>())),
            );
            let mut out = tokio::fs::File::create(&held.0).await.map_err(internal)?;
            let mut total: u64 = 0;
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|_| DomainError::invalid_param("file", "the upload was cut short"))?
            {
                total += chunk.len() as u64;
                if total > limit {
                    return Err(DomainError::SizeLimit { size: total, limit }.into());
                }
                out.write_all(&chunk).await.map_err(internal)?;
            }
            out.flush().await.map_err(internal)?;
            buffered = Some((held, filename));
            continue;
        }
        let value = field
            .text()
            .await
            .map_err(|_| DomainError::invalid_param(&name, "unreadable"))?;
        let value = value.trim().to_string();
        match name.as_str() {
            "title" if !value.is_empty() => title = Some(value),
            "description" if !value.is_empty() => description = Some(value),
            "tags" => tags.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string),
            ),
            "album" if !value.is_empty() => album = Some(value),
            "url" if !value.is_empty() => url = Some(value),
            "nsfw" => {
                nsfw = Some(match value.as_str() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => {
                        return Err(
                            DomainError::invalid_param("nsfw", "must be true or false").into()
                        );
                    }
                });
            }
            "expires_in" if !value.is_empty() => {
                let secs: u64 = value.parse().map_err(|_| {
                    DomainError::invalid_param("expires_in", "must be a number of seconds")
                })?;
                expires_in = (secs > 0).then(|| Duration::from_secs(secs));
            }
            "width" if !value.is_empty() => {
                width = Some(
                    value
                        .parse::<u32>()
                        .ok()
                        .filter(|&w| w > 0)
                        .ok_or_else(|| {
                            DomainError::invalid_param("width", "must be a positive integer")
                        })?,
                );
            }
            _ => {}
        }
    }

    // A file wins over a URL sent alongside it.
    let (held, source) = match (buffered, url) {
        (Some((held, filename)), _) => {
            let reader = tokio::fs::File::open(&held.0).await.map_err(internal)?;
            (
                Some(held),
                UploadSource::Stream {
                    reader: Box::new(reader),
                    filename,
                },
            )
        }
        (None, Some(url)) => (None, UploadSource::Url(url)),
        (None, None) => return Err(DomainError::EmptySource.into()),
    };
    let uploaded = app
        .uploads
        .upload(UploadCommand {
            actor: caller.actor,
            source,
            title,
            description,
            tags,
            album,
            category: None,
            nsfw,
            expiration: expires_in,
            width,
            uploader_ip: ip,
        })
        .await?;
    drop(held);

    let status = if uploaded.duplicate {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    let body = UploadResponse {
        file: FileView::new(&uploaded, true, caller.actor),
        duplicate: uploaded.duplicate,
    };
    Ok((status, Json(body)).into_response())
}

async fn openapi_json() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        crate::openapi::document(),
    )
        .into_response()
}

/// `POST /api/v2/files/{id}/view`: count a view of the file's page. The
/// web app sends it once per visit; plain reads of the file are not views.
#[utoipa::path(post, path = "/api/v2/files/{id}/view", tag = "files",
    params(("id" = String, Path, description = "The file's public id")),
    responses(
        (status = 204, description = "Counted, or already counted for this viewer lately"),
        (status = 404, description = "Not there, or not for this caller", body = crate::problem::ProblemBody),
    ))]
async fn view(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Request,
) -> Result<StatusCode, Problem> {
    let now = Utc::now();
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let caller = identify(&app, &headers, now).await?;
    app.uploads.record_view(caller.actor, ip, &id, now).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn no_such_endpoint() -> Problem {
    DomainError::NotFound("endpoint").into()
}

/// `GET /api/v2/files/{id}`: the file if the caller may see it, else 404.
#[utoipa::path(get, path = "/api/v2/files/{id}", tag = "files",
    params(("id" = String, Path, description = "The file's public id")),
    responses(
        (status = 200, body = FileView),
        (status = 404, description = "Not there, or not for this caller", body = crate::problem::ProblemBody),
    ))]
async fn file(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<FileView>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    let unlocked = app.album_locks.unlocked(&album_grants(&headers), now).await;
    let uploaded = app
        .uploads
        .view_for(caller.actor, &id, now, &unlocked)
        .await?;
    Ok(Json(FileView::new(&uploaded, false, caller.actor)))
}
