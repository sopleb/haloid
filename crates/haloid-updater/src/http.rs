//! The sidecar's API, for Haloid only: every route but `/healthz` wants
//! `Authorization: Bearer <HALOID_UPDATER_TOKEN>`.

use crate::{UpdateError, UpdateRequest, Updater};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub fn router(updater: Arc<Updater>) -> Router {
    Router::new()
        .route("/v1/update", post(update))
        .route("/v1/status", get(status))
        .route_layer(axum::middleware::from_fn_with_state(
            updater.clone(),
            authorised,
        ))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(updater)
}

async fn authorised(
    State(updater): State<Arc<Updater>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let expected = updater.token().as_bytes();
    if presented.as_bytes().ct_eq(expected).into() {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

async fn update(
    State(updater): State<Arc<Updater>>,
    Json(request): Json<UpdateRequest>,
) -> Response {
    match updater.start(request).await {
        Ok(version) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "version": version })),
        )
            .into_response(),
        Err(error) => {
            let status = match error {
                UpdateError::Refused(_) => StatusCode::UNPROCESSABLE_ENTITY,
                UpdateError::Busy => StatusCode::CONFLICT,
                UpdateError::Docker(_) => StatusCode::BAD_GATEWAY,
            };
            (
                status,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn status(State(updater): State<Arc<Updater>>) -> Json<crate::Status> {
    Json(updater.status())
}
