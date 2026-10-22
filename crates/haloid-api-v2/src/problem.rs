//! `DomainError` as RFC 9457 problem details: a real HTTP status, a stable
//! machine-readable `code`, and per-field `errors` when there are several.
//!
//! Unlike the legacy surface, nothing here is bug-compatible. The status
//! says what happened; the `code` is what a client branches on.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use haloid_core::{DomainError, MediaError};
use serde_json::{Map, Value, json};

/// The status and stable code for an error.
pub fn classify(error: &DomainError) -> (StatusCode, &'static str) {
    use DomainError as E;
    use StatusCode as S;
    match error {
        E::MissingApiKey | E::Unauthenticated => (S::UNAUTHORIZED, "unauthenticated"),
        E::InvalidApiKey => (S::UNAUTHORIZED, "invalid_api_key"),
        E::RevokedApiKey => (S::UNAUTHORIZED, "revoked_api_key"),
        E::InvalidCredentials => (S::UNAUTHORIZED, "invalid_credentials"),
        E::Forbidden | E::RoleNotPermitted(_) => (S::FORBIDDEN, "forbidden"),
        E::RegistrationClosed => (S::FORBIDDEN, "registration_closed"),
        E::AlbumLocked => (S::FORBIDDEN, "album_locked"),
        E::ProRequired(_) => (S::FORBIDDEN, "pro_required"),
        E::HumanVerificationFailed => (S::FORBIDDEN, "human_verification_failed"),
        E::AccountLocked { .. } => (S::TOO_MANY_REQUESTS, "account_locked"),
        E::RateLimited { .. } => (S::TOO_MANY_REQUESTS, "rate_limited"),
        E::NotFound(_) => (S::NOT_FOUND, "not_found"),
        E::Conflict(_) => (S::CONFLICT, "conflict"),
        E::Duplicate { .. } => (S::CONFLICT, "duplicate"),
        E::SizeLimit { .. } => (S::PAYLOAD_TOO_LARGE, "too_large"),
        E::PixelLimit { .. } => (S::PAYLOAD_TOO_LARGE, "too_many_pixels"),
        E::InvalidParam { .. } | E::InvalidParams(_) => (S::UNPROCESSABLE_ENTITY, "invalid_params"),
        E::EmptySource => (S::UNPROCESSABLE_ENTITY, "empty_source"),
        E::InvalidBase64 | E::Base64ViaGet => (S::UNPROCESSABLE_ENTITY, "invalid_base64"),
        E::ExtensionNotAllowed(_) => (S::UNPROCESSABLE_ENTITY, "type_not_allowed"),
        E::Media(MediaError::Unsupported(_)) => (S::UNPROCESSABLE_ENTITY, "unsupported_media"),
        E::Media(MediaError::ProbeFailed(_)) => (S::UNPROCESSABLE_ENTITY, "unreadable_media"),
        E::StopWord => (S::UNPROCESSABLE_ENTITY, "forbidden_content"),
        E::UrlFetchRejected(_) => (S::UNPROCESSABLE_ENTITY, "url_rejected"),
        E::UnsupportedApiVersion | E::UnknownAction => (S::NOT_FOUND, "not_found"),
        E::StorageFull => (S::INSUFFICIENT_STORAGE, "storage_full"),
        E::Media(_) | E::Storage(_) | E::Database(_) | E::Internal(_) => {
            (S::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}

/// A problem response.
pub struct Problem(pub DomainError);

impl From<DomainError> for Problem {
    fn from(error: DomainError) -> Self {
        Self(error)
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let error = self.0;
        let (status, code) = classify(&error);
        let detail = if error.is_client_safe() {
            error.to_string()
        } else {
            // The detail goes to the log with the request, never the client.
            tracing::error!(%error, source = ?std::error::Error::source(&error), "request failed");
            "Something went wrong on our side.".to_string()
        };

        let mut body = Map::new();
        body.insert("type".into(), json!("about:blank"));
        body.insert(
            "title".into(),
            json!(status.canonical_reason().unwrap_or("Error")),
        );
        body.insert("status".into(), json!(status.as_u16()));
        body.insert("detail".into(), json!(detail));
        body.insert("code".into(), json!(code));
        let fields: Vec<(String, String)> = match &error {
            DomainError::InvalidParam { field, reason } => vec![(field.clone(), reason.clone())],
            DomainError::InvalidParams(errors) => errors
                .iter()
                .map(|e| (e.field.clone(), e.reason.clone()))
                .collect(),
            _ => Vec::new(),
        };
        if !fields.is_empty() {
            let mut errors = Map::new();
            for (field, reason) in fields {
                let list = errors
                    .entry(field)
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(list) = list {
                    list.push(json!(reason));
                }
            }
            body.insert("errors".into(), Value::Object(errors));
        }

        let mut response = (status, axum::Json(Value::Object(body))).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        let retry_after = match error {
            DomainError::RateLimited { retry_after_secs }
            | DomainError::AccountLocked { retry_after_secs } => Some(retry_after_secs),
            _ => None,
        };
        if let Some(secs) = retry_after
            && let Ok(value) = HeaderValue::from_str(&secs.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haloid_core::FieldError;

    async fn body(problem: Problem) -> (StatusCode, Value, Option<String>) {
        let response = problem.into_response();
        let status = response.status();
        let retry = response
            .headers()
            .get(header::RETRY_AFTER)
            .map(|v| v.to_str().unwrap().to_string());
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap(), retry)
    }

    #[tokio::test]
    async fn problems_carry_status_code_and_field_errors() {
        let (status, json, _) = body(Problem(DomainError::InvalidParams(vec![
            FieldError {
                field: "title".into(),
                reason: "too long".into(),
            },
            FieldError {
                field: "title".into(),
                reason: "has a stop word".into(),
            },
            FieldError {
                field: "nsfw".into(),
                reason: "not a boolean".into(),
            },
        ])))
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(json["status"], 422);
        assert_eq!(json["code"], "invalid_params");
        assert_eq!(
            json["errors"]["title"],
            json!(["too long", "has a stop word"])
        );
        assert_eq!(json["errors"]["nsfw"], json!(["not a boolean"]));
    }

    #[tokio::test]
    async fn rate_limits_say_when_to_retry() {
        let (status, json, retry) = body(Problem(DomainError::RateLimited {
            retry_after_secs: 42,
        }))
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(json["code"], "rate_limited");
        assert_eq!(retry.as_deref(), Some("42"));
    }

    #[tokio::test]
    async fn infrastructure_detail_stays_in_the_log() {
        let (status, json, _) = body(Problem(DomainError::Database(
            "mysql://root:hunter2@db".into(),
        )))
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!json["detail"].as_str().unwrap().contains("hunter2"));
    }
}

/// The problem-details body, for the OpenAPI document. Every v2 error
/// response is one of these with `Content-Type: application/problem+json`.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[allow(dead_code)]
pub struct ProblemBody {
    /// Always `about:blank`; `code` is the machine-readable part.
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    /// Stable, for clients to branch on: `not_found`, `too_large`, …
    pub code: String,
    /// Per-field messages, when the request had invalid fields.
    pub errors: Option<std::collections::BTreeMap<String, Vec<String>>>,
}
