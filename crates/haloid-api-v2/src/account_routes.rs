//! The signed-in user's own account: profile, password, API keys, sessions;
//! and unlocking a password album.

use crate::auth_routes::body;
use crate::caller::{album_cookie, check_csrf, identify, same_origin};
use crate::problem::Problem;
use crate::views::MeView;
use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::account::{ApiKey, Session};
use haloid_core::ids::ApiKeyId;
use haloid_service::app::App;
use haloid_service::auth::account::ProfileEdit;
use serde::{Deserialize, Deserializer, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// What a user may change about themselves. Leave a field out to keep it;
/// `null` clears a display name or email.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfilePatch {
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable)]
    pub display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable)]
    pub email: Option<Option<String>>,
    pub is_private: Option<bool>,
}

/// `PATCH /api/v2/auth/me`.
#[utoipa::path(patch, path = "/api/v2/auth/me", tag = "account",
    request_body = ProfilePatch,
    responses(
        (status = 200, description = "The account as it now is", body = MeView),
        (status = 401, description = "Not signed in", body = crate::problem::ProblemBody),
        (status = 409, description = "That email belongs to another account", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad fields", body = crate::problem::ProblemBody),
    ))]
pub async fn update_me(
    State(app): State<Arc<App>>,
    method: Method,
    headers: HeaderMap,
    patch: Result<Json<ProfilePatch>, JsonRejection>,
) -> Result<Json<MeView>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let patch = body(patch)?;
    let user = app
        .auth
        .update_profile(
            caller.actor,
            ProfileEdit {
                display_name: patch.display_name,
                email: patch.email,
                is_private: patch.is_private,
            },
        )
        .await?;
    Ok(Json(MeView::new(&user)))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordBody {
    pub current: String,
    pub new: String,
}

/// `POST /api/v2/auth/password`: change it, and sign every other session
/// out. A browser session only — an API key cannot change a password.
#[utoipa::path(post, path = "/api/v2/auth/password", tag = "account",
    request_body = PasswordBody,
    responses(
        (status = 204, description = "Changed; other sessions signed out"),
        (status = 401, description = "Not signed in in a browser, or the current password is wrong", body = crate::problem::ProblemBody),
        (status = 422, description = "The new password is too weak", body = crate::problem::ProblemBody),
    ))]
pub async fn change_password(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    request: Result<Json<PasswordBody>, JsonRejection>,
) -> Result<StatusCode, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    same_origin(&headers, &app.settings.get().site.base_url)?;
    let (Some(user), Some(session)) = (caller.actor.user_id(), caller.session.as_ref()) else {
        return Err(DomainError::Unauthenticated.into());
    };
    let request = body(request)?;
    app.auth
        .change_password(user, session, &request.current, &request.new)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ApiKeyView {
    pub id: u64,
    pub label: String,
    /// The start of the key, all that is ever shown again.
    pub prefix: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApiKeyView {
    fn new(key: &ApiKey) -> Self {
        Self {
            id: key.id.0,
            label: key.label.clone(),
            prefix: key.key_prefix.clone(),
            active: key.is_active,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
            revoked_at: key.revoked_at,
        }
    }
}

/// `GET /api/v2/auth/api-keys`: the caller's keys, revoked ones included.
#[utoipa::path(get, path = "/api/v2/auth/api-keys", tag = "account",
    responses(
        (status = 200, body = Vec<ApiKeyView>),
        (status = 401, description = "Not signed in", body = crate::problem::ProblemBody),
    ))]
pub async fn api_keys(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<Vec<ApiKeyView>>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let keys = app.auth.own_api_keys(caller.actor).await?;
    Ok(Json(keys.iter().map(ApiKeyView::new).collect()))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewApiKeyBody {
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CreatedApiKey {
    pub key: ApiKeyView,
    /// The whole key. Shown this once; only its hash is kept.
    pub secret: String,
}

/// `POST /api/v2/auth/api-keys`.
#[utoipa::path(post, path = "/api/v2/auth/api-keys", tag = "account",
    request_body = NewApiKeyBody,
    responses(
        (status = 201, description = "Created; the secret is in this response only", body = CreatedApiKey),
        (status = 401, description = "Not signed in", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad label, or too many keys", body = crate::problem::ProblemBody),
    ))]
pub async fn create_api_key(
    State(app): State<Arc<App>>,
    method: Method,
    headers: HeaderMap,
    request: Result<Json<NewApiKeyBody>, JsonRejection>,
) -> Result<Response, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let request = body(request)?;
    let (key, secret) = app
        .auth
        .create_own_api_key(caller.actor, &request.label)
        .await?;
    let mut response = (
        StatusCode::CREATED,
        Json(CreatedApiKey {
            key: ApiKeyView::new(&key),
            secret,
        }),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `DELETE /api/v2/auth/api-keys/{id}`: revoke one of the caller's keys.
#[utoipa::path(delete, path = "/api/v2/auth/api-keys/{id}", tag = "account",
    params(("id" = u64, Path)),
    responses(
        (status = 204, description = "Revoked"),
        (status = 404, description = "Not one of the caller's keys", body = crate::problem::ProblemBody),
    ))]
pub async fn revoke_api_key(
    State(app): State<Arc<App>>,
    Path(id): Path<u64>,
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
    app.auth
        .revoke_own_api_key(caller.actor, ApiKeyId(id), now)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct LoginSessionView {
    pub id: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// The session making this request.
    pub current: bool,
}

/// `GET /api/v2/auth/sessions`: where the caller is signed in.
#[utoipa::path(get, path = "/api/v2/auth/sessions", tag = "account",
    responses(
        (status = 200, body = Vec<LoginSessionView>),
        (status = 401, description = "Not signed in", body = crate::problem::ProblemBody),
    ))]
pub async fn sessions(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<Vec<LoginSessionView>>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let current = caller.session.as_ref().map(|s| s.as_str().to_string());
    let list: Vec<Session> = app.auth.own_sessions(caller.actor).await?;
    Ok(Json(
        list.iter()
            .map(|s| LoginSessionView {
                id: s.id.as_str().to_string(),
                ip: s.ip.map(|ip| ip.to_string()),
                user_agent: s.user_agent.clone(),
                created_at: s.created_at,
                last_seen_at: s.last_seen_at,
                expires_at: s.expires_at,
                current: current.as_deref() == Some(s.id.as_str()),
            })
            .collect(),
    ))
}

/// `DELETE /api/v2/auth/sessions/{id}`: sign one session out.
#[utoipa::path(delete, path = "/api/v2/auth/sessions/{id}", tag = "account",
    params(("id" = String, Path)),
    responses(
        (status = 204, description = "Signed out"),
        (status = 404, description = "Not one of the caller's sessions", body = crate::problem::ProblemBody),
    ))]
pub async fn revoke_session(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<StatusCode, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    app.auth.revoke_own_session(caller.actor, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UnlockBody {
    pub password: String,
}

/// `POST /api/v2/albums/{id}/unlock`: prove a password album's password;
/// the answer sets a cookie that unlocks it for a day. From our own page
/// only, like signing in.
#[utoipa::path(post, path = "/api/v2/albums/{id}/unlock", tag = "albums",
    params(("id" = String, Path)),
    request_body = UnlockBody,
    responses(
        (status = 204, description = "Unlocked; the grant cookie is set"),
        (status = 401, description = "Wrong password", body = crate::problem::ProblemBody),
        (status = 404, description = "No such password album", body = crate::problem::ProblemBody),
        (status = 429, description = "Too many tries", body = crate::problem::ProblemBody),
    ))]
pub async fn unlock_album(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, Problem> {
    let base_url = app.settings.get().site.base_url.clone();
    same_origin(&headers, &base_url)?;
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let parsed = body(
        <Json<UnlockBody> as axum::extract::FromRequest<()>>::from_request(request, &()).await,
    )?;
    let (grant, lifetime) = app
        .album_locks
        .unlock(&id, &parsed.password, ip, Utc::now())
        .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(value) =
        HeaderValue::from_str(&album_cookie(&base_url, &id, &grant, lifetime.as_secs()))
    {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(response)
}
