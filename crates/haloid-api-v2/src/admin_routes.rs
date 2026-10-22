//! Moderation and administration: the queue, settings, users, storages,
//! deleted files and the audit log. Every write is audited by the service.

use crate::auth_routes::body;
use crate::caller::{check_csrf, identify};
use crate::problem::Problem;
use crate::views::{FileView, PageView};
use axum::extract::{ConnectInfo, Query, Request};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::{HeaderMap, Method};
use axum::response::{IntoResponse, Json};
use chrono::Utc;
use haloid_core::DomainError;
use haloid_core::account::{User, UserStatus};
use haloid_core::auth::Role;
use haloid_core::ids::StorageId;
use haloid_core::storage::{StorageKind, StoragePathMode, StorageRecord};
use haloid_service::admin::By;
use haloid_service::app::App;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct StorageCheckView {
    /// `write`, `read`, `public_url`, `quarantine`, `quarantine_private`,
    /// `release` or `delete`.
    pub name: &'static str,
    pub ok: bool,
    /// What happened, in words an operator can act on.
    pub detail: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct StorageTestView {
    /// Every check passed.
    pub ok: bool,
    pub checks: Vec<StorageCheckView>,
}

/// `POST /api/v2/storages/{id}/test`: write, read, serve, quarantine,
/// release and delete a probe object, reporting each step. Administrators
/// only.
#[utoipa::path(post, path = "/api/v2/storages/{id}/test", tag = "admin",
    params(("id" = u64, Path, description = "The storage's numeric id")),
    responses(
        (status = 200, description = "The checks, passed or not", body = StorageTestView),
        (status = 401, description = "Not signed in", body = crate::problem::ProblemBody),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 404, description = "No such storage", body = crate::problem::ProblemBody),
    ))]
pub async fn test_storage(
    State(app): State<Arc<App>>,
    Path(id): Path<u64>,
    method: Method,
    headers: HeaderMap,
) -> Result<Json<StorageTestView>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let result = app.storage_admin.test(caller.actor, StorageId(id)).await?;
    Ok(Json(StorageTestView {
        ok: result.ok,
        checks: result
            .checks
            .into_iter()
            .map(|c| StorageCheckView {
                name: c.name,
                ok: c.ok,
                detail: c.detail,
            })
            .collect(),
    }))
}

fn ip_of(request: &Request) -> Option<std::net::IpAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

/// Identify the caller, apply the CSRF rule, and note where they are, for
/// the audit log. Takes what it needs from the request up front: a request
/// body is not `Sync`, so a borrow of it cannot be held across an await.
fn acting(app: &App, request: &Request) -> impl Future<Output = Result<By, Problem>> + Send {
    let headers = request.headers().clone();
    let method = request.method().clone();
    let ip = ip_of(request);
    let base_url = app.settings.get().site.base_url.clone();
    async move {
        let caller = identify(app, &headers, Utc::now()).await?;
        check_csrf(&caller, &method, &headers, &base_url)?;
        Ok(By {
            actor: caller.actor,
            ip,
        })
    }
}

async fn json<T: serde::de::DeserializeOwned>(request: Request) -> Result<T, Problem> {
    body(<Json<T> as axum::extract::FromRequest<()>>::from_request(request, &()).await)
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct PageParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct QueuedFileView {
    pub file: FileView,
    pub reason: Option<String>,
    pub queued_at: chrono::DateTime<Utc>,
}

/// `GET /api/v2/moderation/queue`: files held for review, oldest first.
/// Moderators and administrators.
#[utoipa::path(get, path = "/api/v2/moderation/queue", tag = "admin", params(PageParams),
    responses(
        (status = 200, body = PageView<QueuedFileView>),
        (status = 403, description = "Not a moderator", body = crate::problem::ProblemBody),
    ))]
pub async fn moderation_queue(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(params): Query<PageParams>,
) -> Result<Json<PageView<QueuedFileView>>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let (items, next) = app
        .admin
        .queue(caller.actor, params.cursor.as_deref(), params.limit)
        .await?;
    Ok(Json(PageView {
        items: items
            .iter()
            .map(|q| QueuedFileView {
                file: FileView::listed(&q.listed, caller.actor),
                reason: q.reason.clone(),
                queued_at: q.queued_at,
            })
            .collect(),
        next,
    }))
}

/// `POST /api/v2/moderation/{id}/approve`: publish a held file.
#[utoipa::path(post, path = "/api/v2/moderation/{id}/approve", tag = "admin",
    params(("id" = String, Path)),
    responses(
        (status = 204, description = "Approved and published"),
        (status = 403, description = "Not a moderator", body = crate::problem::ProblemBody),
        (status = 404, description = "No such file", body = crate::problem::ProblemBody),
    ))]
pub async fn approve(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    request: Request,
) -> Result<StatusCode, Problem> {
    let by = acting(&app, &request).await?;
    app.admin.approve(by, &id, Utc::now()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DenyBody {
    /// Why, for the audit log.
    pub note: Option<String>,
}

/// `POST /api/v2/moderation/{id}/deny`: refuse a held file, which deletes
/// it with reason `moderation`.
#[utoipa::path(post, path = "/api/v2/moderation/{id}/deny", tag = "admin",
    params(("id" = String, Path)), request_body = DenyBody,
    responses(
        (status = 204, description = "Denied and deleted"),
        (status = 403, description = "Not a moderator", body = crate::problem::ProblemBody),
        (status = 404, description = "No such file", body = crate::problem::ProblemBody),
    ))]
pub async fn deny(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    request: Request,
) -> Result<StatusCode, Problem> {
    let by = acting(&app, &request).await?;
    let has_body = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .is_some();
    let note = if has_body {
        json::<DenyBody>(request).await?.note
    } else {
        None
    };
    app.admin
        .deny(
            by,
            &id,
            note.as_deref().filter(|n| !n.trim().is_empty()),
            Utc::now(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SettingView {
    pub key: String,
    pub value: String,
    pub default: String,
    /// Set in the environment, and read-only here.
    pub pinned: bool,
}

fn setting_views(list: Vec<haloid_service::admin::SettingView>) -> Vec<SettingView> {
    list.into_iter()
        .map(|s| SettingView {
            key: s.key,
            value: s.value,
            default: s.default,
            pinned: s.pinned,
        })
        .collect()
}

/// `GET /api/v2/settings`: every setting, its value, default and whether
/// the environment pins it. Administrators.
#[utoipa::path(get, path = "/api/v2/settings", tag = "admin",
    responses(
        (status = 200, body = Vec<SettingView>),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn settings(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<Vec<SettingView>>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    Ok(Json(setting_views(app.admin.settings(caller.actor)?)))
}

/// `PATCH /api/v2/settings`, a map of key to new value. All or nothing: an
/// unknown key, a bad value or a pinned key refuses the whole write. Live
/// before the response.
#[utoipa::path(patch, path = "/api/v2/settings", tag = "admin",
    request_body = BTreeMap<String, String>,
    responses(
        (status = 200, description = "Every setting, as it now is", body = Vec<SettingView>),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 422, description = "Refused; `errors` says which keys and why", body = crate::problem::ProblemBody),
    ))]
pub async fn update_settings(
    State(app): State<Arc<App>>,
    request: Request,
) -> Result<Json<Vec<SettingView>>, Problem> {
    let by = acting(&app, &request).await?;
    let changes: BTreeMap<String, String> = json(request).await?;
    Ok(Json(setting_views(
        app.admin.update_settings(by, &changes).await?,
    )))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AdminUserView {
    pub id: String,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    /// `user`, `manager` or `admin`.
    pub role: &'static str,
    /// `valid`, `awaiting_confirmation` or `banned`.
    pub status: &'static str,
    pub is_private: bool,
    pub file_count: u32,
    pub disk_used: u64,
    pub created_at: chrono::DateTime<Utc>,
    pub last_login_at: Option<chrono::DateTime<Utc>>,
}

impl AdminUserView {
    fn new(user: &User) -> Self {
        Self {
            id: user.id_encoded.to_string(),
            username: user.username.clone(),
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            role: user.role.as_str(),
            status: user.status.as_str(),
            is_private: user.is_private,
            file_count: user.file_count,
            disk_used: user.disk_used,
            created_at: user.created_at,
            last_login_at: user.last_login_at,
        }
    }
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct UserListParams {
    /// Part of a username or email.
    pub search: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// `GET /api/v2/admin/users`. Administrators.
#[utoipa::path(get, path = "/api/v2/admin/users", tag = "admin", params(UserListParams),
    responses(
        (status = 200, body = PageView<AdminUserView>),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn users(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(params): Query<UserListParams>,
) -> Result<Json<PageView<AdminUserView>>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let (list, next) = app
        .admin
        .users(
            caller.actor,
            params.search.as_deref(),
            params.cursor.as_deref(),
            params.limit,
        )
        .await?;
    Ok(Json(PageView {
        items: list.iter().map(AdminUserView::new).collect(),
        next,
    }))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserPatch {
    pub role: Option<String>,
    pub status: Option<String>,
}

/// `PATCH /api/v2/admin/users/{id}`: role and status. A ban signs the user
/// out everywhere. No one can demote or ban themselves.
#[utoipa::path(patch, path = "/api/v2/admin/users/{id}", tag = "admin",
    params(("id" = String, Path)), request_body = UserPatch,
    responses(
        (status = 200, body = AdminUserView),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 404, description = "No such user", body = crate::problem::ProblemBody),
        (status = 422, description = "A bad role or status, or a change to yourself", body = crate::problem::ProblemBody),
    ))]
pub async fn update_user(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Json<AdminUserView>, Problem> {
    let by = acting(&app, &request).await?;
    let patch: UserPatch = json(request).await?;
    let role = patch
        .role
        .as_deref()
        .map(|r| {
            r.parse::<Role>()
                .map_err(|_| DomainError::invalid_param("role", "one of user, manager, admin"))
        })
        .transpose()?;
    let status = patch
        .status
        .as_deref()
        .map(|s| {
            s.parse::<UserStatus>().map_err(|_| {
                DomainError::invalid_param("status", "one of valid, awaiting_confirmation, banned")
            })
        })
        .transpose()?;
    let user = app.admin.update_user(by, &id, role, status).await?;
    Ok(Json(AdminUserView::new(&user)))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct StorageView {
    pub id: u64,
    pub name: String,
    /// `local` or `s3`.
    pub kind: &'static str,
    /// The kind's own settings: bucket, endpoint, paths. Never credentials.
    #[schema(value_type = Object)]
    pub config: serde_json::Value,
    pub url_prefix: String,
    /// `datefolder`, `direct` or `path`.
    pub path_mode: &'static str,
    pub path_template: Option<String>,
    pub capacity_bytes: Option<u64>,
    pub used_bytes: u64,
    pub file_count: u64,
    pub is_active: bool,
    pub priority: i32,
    pub retired_at: Option<chrono::DateTime<Utc>>,
}

impl StorageView {
    fn new(record: &StorageRecord) -> Self {
        Self {
            id: record.id.0,
            name: record.name.clone(),
            kind: record.kind.as_str(),
            config: serde_json::from_str(&record.config).unwrap_or(serde_json::Value::Null),
            url_prefix: record.url_prefix.clone(),
            path_mode: record.path_mode.as_str(),
            path_template: record.path_template.clone(),
            capacity_bytes: record.capacity_bytes,
            used_bytes: record.used_bytes,
            file_count: record.file_count,
            is_active: record.is_active,
            priority: record.priority,
            retired_at: record.retired_at,
        }
    }
}

/// `GET /api/v2/storages`. Administrators.
#[utoipa::path(get, path = "/api/v2/storages", tag = "admin",
    responses(
        (status = 200, body = Vec<StorageView>),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn storages(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<Vec<StorageView>>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let list = app.admin.storages(caller.actor).await?;
    Ok(Json(list.iter().map(StorageView::new).collect()))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageBody {
    pub name: String,
    /// `local` or `s3`.
    pub kind: String,
    #[schema(value_type = Object)]
    pub config: serde_json::Value,
    pub url_prefix: String,
    /// `datefolder` (the default), `direct` or `path`.
    pub path_mode: Option<String>,
    pub path_template: Option<String>,
    pub capacity_bytes: Option<u64>,
    #[serde(default = "yes")]
    pub is_active: bool,
    #[serde(default)]
    pub priority: i32,
}

fn yes() -> bool {
    true
}

impl StorageBody {
    fn details(self) -> Result<haloid_service::admin::StorageDetails, DomainError> {
        Ok(haloid_service::admin::StorageDetails {
            name: self.name.trim().to_string(),
            kind: self
                .kind
                .parse::<StorageKind>()
                .map_err(|()| DomainError::invalid_param("kind", "local or s3"))?,
            config: self.config.to_string(),
            url_prefix: self.url_prefix.trim().trim_end_matches('/').to_string(),
            path_mode: match self.path_mode.as_deref() {
                None => StoragePathMode::DateFolder,
                Some(mode) => mode.parse::<StoragePathMode>().map_err(|()| {
                    DomainError::invalid_param("path_mode", "datefolder, direct or path")
                })?,
            },
            path_template: self.path_template.filter(|t| !t.trim().is_empty()),
            capacity_bytes: self.capacity_bytes,
            is_active: self.is_active,
            priority: self.priority,
        })
    }
}

/// `POST /api/v2/storages`: add a storage. Checked first, and in use as
/// soon as it is saved.
#[utoipa::path(post, path = "/api/v2/storages", tag = "admin", request_body = StorageBody,
    responses(
        (status = 201, body = StorageView),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 409, description = "The name is taken", body = crate::problem::ProblemBody),
        (status = 422, description = "A configuration that cannot work", body = crate::problem::ProblemBody),
    ))]
pub async fn create_storage(
    State(app): State<Arc<App>>,
    request: Request,
) -> Result<axum::response::Response, Problem> {
    let by = acting(&app, &request).await?;
    let details = json::<StorageBody>(request).await?.details()?;
    let created = app.admin.create_storage(by, &details).await?;
    Ok((StatusCode::CREATED, Json(StorageView::new(&created))).into_response())
}

/// `PATCH /api/v2/storages/{id}`: replace a storage's settings. Takes
/// effect as soon as it is saved.
#[utoipa::path(patch, path = "/api/v2/storages/{id}", tag = "admin",
    params(("id" = u64, Path)), request_body = StorageBody,
    responses(
        (status = 200, body = StorageView),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 404, description = "No such storage", body = crate::problem::ProblemBody),
        (status = 422, description = "A configuration that cannot work", body = crate::problem::ProblemBody),
    ))]
pub async fn update_storage(
    State(app): State<Arc<App>>,
    Path(id): Path<u64>,
    request: Request,
) -> Result<Json<StorageView>, Problem> {
    let by = acting(&app, &request).await?;
    let details = json::<StorageBody>(request).await?.details()?;
    let updated = app
        .admin
        .update_storage(by, StorageId(id), &details)
        .await?;
    Ok(Json(StorageView::new(&updated)))
}

/// `POST /api/v2/storages/{id}/retire`: no new uploads; it keeps serving.
#[utoipa::path(post, path = "/api/v2/storages/{id}/retire", tag = "admin",
    params(("id" = u64, Path)),
    responses(
        (status = 204, description = "Retired"),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 404, description = "No such storage", body = crate::problem::ProblemBody),
    ))]
pub async fn retire_storage(
    State(app): State<Arc<App>>,
    Path(id): Path<u64>,
    request: Request,
) -> Result<StatusCode, Problem> {
    let by = acting(&app, &request).await?;
    app.admin
        .retire_storage(by, StorageId(id), Utc::now())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v2/admin/files/deleted`: deleted files, retained until purged.
#[utoipa::path(get, path = "/api/v2/admin/files/deleted", tag = "admin", params(PageParams),
    responses(
        (status = 200, body = PageView<FileView>),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn deleted_files(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(params): Query<PageParams>,
) -> Result<Json<PageView<FileView>>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    let page = app
        .admin
        .deleted_files(caller.actor, params.cursor.as_deref(), params.limit, now)
        .await?;
    Ok(Json(PageView::map(page, |f| {
        FileView::listed(f, caller.actor)
    })))
}

/// `POST /api/v2/admin/files/{id}/restore`.
#[utoipa::path(post, path = "/api/v2/admin/files/{id}/restore", tag = "admin",
    params(("id" = String, Path)),
    responses(
        (status = 204, description = "Restored"),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 404, description = "No such file", body = crate::problem::ProblemBody),
    ))]
pub async fn restore_file(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    request: Request,
) -> Result<StatusCode, Problem> {
    let by = acting(&app, &request).await?;
    app.admin.restore(by, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v2/admin/files/{id}/purge`: destroy row and bytes, by a job
/// that retries if storage fails partway.
#[utoipa::path(post, path = "/api/v2/admin/files/{id}/purge", tag = "admin",
    params(("id" = String, Path)),
    responses(
        (status = 202, description = "Queued for destruction"),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 404, description = "No such file", body = crate::problem::ProblemBody),
    ))]
pub async fn purge_file(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    request: Request,
) -> Result<StatusCode, Problem> {
    let by = acting(&app, &request).await?;
    app.admin.purge(by, &id, Utc::now()).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AuditEntryView {
    pub id: u64,
    pub actor: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub target_id: Option<u64>,
    #[schema(value_type = Object)]
    pub detail: serde_json::Value,
    pub ip: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct AuditParams {
    /// Entries older than this id.
    pub before: Option<u64>,
    pub limit: Option<u32>,
}

/// `GET /api/v2/admin/audit`: newest first.
#[utoipa::path(get, path = "/api/v2/admin/audit", tag = "admin", params(AuditParams),
    responses(
        (status = 200, body = Vec<AuditEntryView>),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn audit_log(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(params): Query<AuditParams>,
) -> Result<Json<Vec<AuditEntryView>>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let entries = app
        .admin
        .audit_log(caller.actor, params.before, params.limit)
        .await?;
    Ok(Json(
        entries
            .into_iter()
            .map(|e| AuditEntryView {
                id: e.id,
                actor: e.actor_username,
                action: e.action,
                target: e.target,
                target_id: e.target_id,
                detail: e.detail,
                ip: e.ip.map(|ip| ip.to_string()),
                created_at: e.created_at,
            })
            .collect(),
    ))
}

/// `GET /api/v2/settings/watermark`: the current watermark image.
#[utoipa::path(get, path = "/api/v2/settings/watermark", tag = "admin",
    responses(
        (status = 200, description = "The image", content_type = "image/*"),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 404, description = "No watermark is set", body = crate::problem::ProblemBody),
    ))]
pub async fn watermark(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<axum::response::Response, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let (bytes, mime) = app
        .admin
        .watermark(caller.actor)
        .await?
        .ok_or(DomainError::NotFound("watermark"))?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
        ],
        bytes,
    )
        .into_response())
}

/// `PUT /api/v2/settings/watermark`, the image as the body: set the mark
/// that `watermark.enabled` stamps. A still image, at most 5 MiB.
#[utoipa::path(put, path = "/api/v2/settings/watermark", tag = "admin",
    request_body(content = Vec<u8>, content_type = "image/png"),
    responses(
        (status = 204, description = "Set"),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 413, description = "Over 5 MiB", body = crate::problem::ProblemBody),
        (status = 422, description = "Not a still image", body = crate::problem::ProblemBody),
    ))]
pub async fn set_watermark(
    State(app): State<Arc<App>>,
    request: Request,
) -> Result<StatusCode, Problem> {
    let by = acting(&app, &request).await?;
    let limit = haloid_service::admin::Admin::MAX_WATERMARK_BYTES;
    let bytes = axum::body::to_bytes(request.into_body(), limit + 1)
        .await
        .map_err(|_| DomainError::SizeLimit {
            size: limit as u64 + 1,
            limit: limit as u64,
        })?;
    app.admin.set_watermark(by, &bytes).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/v2/settings/watermark`.
#[utoipa::path(delete, path = "/api/v2/settings/watermark", tag = "admin",
    responses(
        (status = 204, description = "Removed; nothing is stamped until another is set"),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn remove_watermark(
    State(app): State<Arc<App>>,
    request: Request,
) -> Result<StatusCode, Problem> {
    let by = acting(&app, &request).await?;
    app.admin.remove_watermark(by).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v2/metrics`: Prometheus text format. For an administrator, or
/// for a scraper holding `HALOID_METRICS_TOKEN` as its bearer token.
#[utoipa::path(get, path = "/api/v2/metrics", tag = "admin",
    responses(
        (status = 200, description = "Metrics", content_type = "text/plain"),
        (status = 401, description = "No admin key and no metrics token", body = crate::problem::ProblemBody),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn metrics(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<axum::response::Response, Problem> {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let by_token = match (app.metrics.token(), bearer) {
        (Some(token), Some(given)) => haloid_service::auth::secrets::hashes_match(token, given),
        _ => false,
    };
    if !by_token {
        let caller = identify(&app, &headers, Utc::now()).await?;
        if caller.actor.is_guest() {
            return Err(DomainError::Unauthenticated.into());
        }
        if !caller.actor.is_admin() {
            return Err(DomainError::Forbidden.into());
        }
    }
    let text = app.metrics_text().await?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        text,
    )
        .into_response())
}

// Licence

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct LicenceView {
    /// This instance's id: what a key is issued for.
    pub instance_id: String,
    /// `none`, `active` or `lapsed`.
    pub state: &'static str,
    /// Whether Pro features may be put to new use.
    pub pro: bool,
    pub branding: bool,
    pub licence: Option<LicenceDetailView>,
    /// Set by `HALOID_LICENCE_KEY`, so not changeable here.
    pub from_environment: bool,
    /// The kinds of storage this build can use: `local`, and `s3` in Pro.
    pub storage_kinds: Vec<&'static str>,
    /// Storages this build cannot load, by name: they need the Pro build.
    pub unavailable_storages: Vec<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct LicenceDetailView {
    pub id: String,
    pub licensee: String,
    /// `trial`, `standard` or `perpetual`.
    pub kind: &'static str,
    pub issued_at: chrono::DateTime<Utc>,
    pub expires_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct LicenceBody {
    /// The key, as issued: `HALOID1.…`.
    pub key: String,
}

fn licence_view(app: &App, status: haloid_service::licensing::LicenceStatus) -> LicenceView {
    LicenceView {
        instance_id: status.instance_id,
        state: status.entitlements.state.as_str(),
        pro: status.entitlements.pro,
        branding: status.entitlements.branding,
        licence: status.licence.map(|l| LicenceDetailView {
            id: l.id,
            licensee: l.licensee,
            kind: l.kind.as_str(),
            issued_at: l.issued_at,
            expires_at: l.expires_at,
        }),
        from_environment: status.from_environment,
        storage_kinds: app.storages.kinds().iter().map(|k| k.as_str()).collect(),
        unavailable_storages: app.storages.unavailable().to_vec(),
    }
}

/// `GET /api/v2/licence`: the instance id and the licence. Administrators.
#[utoipa::path(get, path = "/api/v2/licence", tag = "admin",
    responses(
        (status = 200, body = LicenceView),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn licence(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<LicenceView>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    let status = app.licensing.status(caller.actor, now)?;
    Ok(Json(licence_view(&app, status)))
}

/// `PUT /api/v2/licence`: install a key. It must be signed by Haloid and
/// issued for this instance; it applies at once.
#[utoipa::path(put, path = "/api/v2/licence", tag = "admin", request_body = LicenceBody,
    responses(
        (status = 200, body = LicenceView),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 422, description = "Not a valid key for this instance", body = crate::problem::ProblemBody),
    ))]
pub async fn install_licence(
    State(app): State<Arc<App>>,
    request: Request,
) -> Result<Json<LicenceView>, Problem> {
    let by = acting(&app, &request).await?;
    let LicenceBody { key } = json(request).await?;
    let status = app.licensing.install(by, &key, Utc::now()).await?;
    Ok(Json(licence_view(&app, status)))
}

/// `DELETE /api/v2/licence`: remove the installed key.
#[utoipa::path(delete, path = "/api/v2/licence", tag = "admin",
    responses(
        (status = 204, description = "Removed; the instance runs as Community"),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn remove_licence(
    State(app): State<Arc<App>>,
    request: Request,
) -> Result<StatusCode, Problem> {
    let by = acting(&app, &request).await?;
    app.licensing.remove(by).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Updates

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UpdateView {
    /// False when this build cannot update itself (in a container, the
    /// image is upgraded instead); then nothing else here applies.
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    /// The running version.
    pub current: String,
    /// `community` or `pro`.
    pub edition: &'static str,
    /// The `updates.mode` setting: `off`, `notify` or `auto`.
    pub mode: &'static str,
    /// The `updates.hour` setting: when `auto` installs, UTC.
    pub hour: u8,
    pub available: Option<AvailableView>,
    pub checked_at: Option<chrono::DateTime<Utc>>,
    /// Why the last check failed.
    pub error: Option<String>,
    /// Whether a release can be installed here.
    pub installable: bool,
    /// If not, why not.
    pub not_installable: Option<String>,
    /// `idle`, `downloading`, `installing`, `restarting` or `failed`.
    pub phase: &'static str,
    /// What went wrong, when `phase` is `failed`.
    pub failure: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AvailableView {
    pub version: String,
    pub notes_url: Option<String>,
    pub published_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct InstallingView {
    /// The version being installed.
    pub version: String,
}

fn update_view(status: haloid_service::updates::UpdateStatus) -> UpdateView {
    use haloid_service::updates::Phase;
    UpdateView {
        enabled: status.enabled,
        disabled_reason: status.disabled_reason,
        current: status.current,
        edition: status.edition.as_str(),
        mode: status.mode.as_str(),
        hour: status.hour,
        available: status.available.map(|a| AvailableView {
            version: a.version,
            notes_url: a.notes_url,
            published_at: a.published_at,
        }),
        checked_at: status.checked_at,
        error: status.error,
        installable: status.installable.is_ok(),
        not_installable: status.installable.err(),
        phase: status.phase.as_str(),
        failure: match status.phase {
            Phase::Failed(reason) => Some(reason),
            _ => None,
        },
    }
}

/// `GET /api/v2/updates`: this build, the release on offer, and whether it
/// can be installed. Administrators.
#[utoipa::path(get, path = "/api/v2/updates", tag = "admin",
    responses(
        (status = 200, body = UpdateView),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn updates(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<UpdateView>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    Ok(Json(update_view(app.updates.status(caller.actor)?)))
}

/// `POST /api/v2/updates/check`: look for a release now. A failed look is
/// reported in `error`, not as an error response.
#[utoipa::path(post, path = "/api/v2/updates/check", tag = "admin",
    responses(
        (status = 200, body = UpdateView),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
    ))]
pub async fn check_updates(
    State(app): State<Arc<App>>,
    request: Request,
) -> Result<Json<UpdateView>, Problem> {
    let by = acting(&app, &request).await?;
    // Authorise before reaching out to the release host.
    app.updates.status(by.actor)?;
    let _ = app.updates.check().await;
    Ok(Json(update_view(app.updates.status(by.actor)?)))
}

/// `POST /api/v2/updates/install`: install the release on offer. Answers
/// once it has started; the server then restarts into it. Back up the
/// database first: migrations run on start and are not undone by a
/// rollback.
#[utoipa::path(post, path = "/api/v2/updates/install", tag = "admin",
    responses(
        (status = 202, body = InstallingView),
        (status = 403, description = "Not an administrator", body = crate::problem::ProblemBody),
        (status = 409, description = "An update is already under way", body = crate::problem::ProblemBody),
        (status = 422, description = "Nothing on offer, or this install cannot update itself", body = crate::problem::ProblemBody),
    ))]
pub async fn install_update(
    State(app): State<Arc<App>>,
    request: Request,
) -> Result<(StatusCode, Json<InstallingView>), Problem> {
    let by = acting(&app, &request).await?;
    let version = app.updates.install(by).await?;
    Ok((StatusCode::ACCEPTED, Json(InstallingView { version })))
}
