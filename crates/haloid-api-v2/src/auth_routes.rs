//! `/api/v2/auth/*`: sign-in for the browser.
//!
//! Login and registration take the ALTCHA widget's payload, and must come
//! from our own page (`same_origin`) whoever the caller, so that another
//! site cannot sign a visitor into an account of its choosing. A login sets
//! the session cookie; the token in it is never stored, only its hash.

use crate::caller::{identify, same_origin, session_cookie};
use crate::problem::Problem;
use crate::views::MeView;
use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use haloid_core::DomainError;
use haloid_service::app::App;
use haloid_service::auth::{LoginRequest, RegisterRequest};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct LoginBody {
    pub login: String,
    pub password: String,
    pub altcha: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RegisterBody {
    pub username: String,
    pub email: Option<String>,
    pub password: String,
    pub altcha: String,
}

/// A malformed JSON body as a problem, not axum's plain-text rejection.
pub fn body<T>(parsed: Result<Json<T>, JsonRejection>) -> Result<T, Problem> {
    parsed
        .map(|Json(value)| value)
        .map_err(|rejection| DomainError::invalid_param("body", rejection.body_text()).into())
}

fn ip_of(request: &Request) -> Option<std::net::IpAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

/// `GET /api/v2/auth/challenge`: a proof-of-work challenge for the widget.
/// Rate limited per address: making one costs the server a key derivation.
#[utoipa::path(get, path = "/api/v2/auth/challenge", tag = "auth",
    responses(
        (status = 200, description = "A signed ALTCHA v2 challenge for the widget", body = serde_json::Value),
        (status = 429, description = "Too many challenges from this address", body = crate::problem::ProblemBody),
    ))]
pub async fn challenge(State(app): State<Arc<App>>, request: Request) -> Result<Response, Problem> {
    let challenge = app.auth.challenge(ip_of(&request), Utc::now())?;
    let mut response = Json(challenge).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[utoipa::path(post, path = "/api/v2/auth/register", tag = "auth",
    request_body = RegisterBody,
    responses(
        (status = 201, description = "The new account", body = MeView),
        (status = 403, description = "Registration closed, failed proof-of-work, or not our page", body = crate::problem::ProblemBody),
        (status = 409, description = "Username or email taken", body = crate::problem::ProblemBody),
        (status = 422, description = "Invalid username, email or password", body = crate::problem::ProblemBody),
        (status = 429, description = "Too many registrations from this address", body = crate::problem::ProblemBody),
    ))]
pub async fn register(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, Problem> {
    same_origin(&headers, &app.settings.get().site.base_url)?;
    let ip = ip_of(&request);
    let parsed = body(Json::<RegisterBody>::from_request_json(request).await)?;
    let user = app
        .auth
        .register(
            &RegisterRequest {
                username: &parsed.username,
                email: parsed.email.as_deref().filter(|e| !e.trim().is_empty()),
                password: &parsed.password,
                altcha: &parsed.altcha,
                ip,
            },
            Utc::now(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(MeView::new(&user))).into_response())
}

#[utoipa::path(post, path = "/api/v2/auth/login", tag = "auth",
    request_body = LoginBody,
    responses(
        (status = 200, description = "Signed in; the session cookie is set", body = MeView),
        (status = 401, description = "Wrong login or password", body = crate::problem::ProblemBody),
        (status = 403, description = "Failed proof-of-work, banned, or not our page", body = crate::problem::ProblemBody),
        (status = 429, description = "Rate limited or account locked", body = crate::problem::ProblemBody),
    ))]
pub async fn login(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, Problem> {
    let base_url = app.settings.get().site.base_url.clone();
    same_origin(&headers, &base_url)?;
    let ip = ip_of(&request);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let parsed = body(Json::<LoginBody>::from_request_json(request).await)?;
    let logged_in = app
        .auth
        .login(
            &LoginRequest {
                login: &parsed.login,
                password: &parsed.password,
                altcha: &parsed.altcha,
                ip,
                user_agent: user_agent.as_deref(),
            },
            Utc::now(),
        )
        .await?;

    let cookie = session_cookie(
        &base_url,
        Some(&logged_in.token),
        app.auth.session_lifetime().as_secs(),
    );
    let mut response = Json(MeView::new(&logged_in.user)).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(response)
}

/// `POST /api/v2/auth/logout`: end this session, and clear the cookie
/// whether or not there was one.
#[utoipa::path(post, path = "/api/v2/auth/logout", tag = "auth",
    responses((status = 204, description = "Signed out; the session cookie is cleared")))]
pub async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Response, Problem> {
    let base_url = app.settings.get().site.base_url.clone();
    let caller = identify(&app, &headers, Utc::now()).await?;
    if caller.via_cookie {
        same_origin(&headers, &base_url)?;
    }
    if let Some(session) = &caller.session {
        app.auth.logout(session).await?;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(value) = HeaderValue::from_str(&session_cookie(&base_url, None, 0)) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(response)
}

/// `GET /api/v2/auth/me`: the signed-in user, or 401.
#[utoipa::path(get, path = "/api/v2/auth/me", tag = "auth",
    responses(
        (status = 200, description = "The signed-in user", body = MeView),
        (status = 401, description = "Not signed in", body = crate::problem::ProblemBody),
    ))]
pub async fn me(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<MeView>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let user = app
        .auth
        .user(caller.actor)
        .await?
        .ok_or(DomainError::Unauthenticated)?;
    Ok(Json(MeView::new(&user)))
}

/// `Json::from_request` without the state argument this crate never uses.
trait FromRequestJson: Sized {
    async fn from_request_json(request: Request) -> Result<Self, JsonRejection>;
}

impl<T: serde::de::DeserializeOwned> FromRequestJson for Json<T> {
    async fn from_request_json(request: Request) -> Result<Self, JsonRejection> {
        <Json<T> as axum::extract::FromRequest<()>>::from_request(request, &()).await
    }
}
