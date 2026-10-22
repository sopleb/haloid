//! The one error enum every layer speaks.
//!
//! Both API surfaces map this to their own wire format in their own crate:
//! `haloid-api-legacy` to the legacy numeric codes with a permanent HTTP 400,
//! `haloid-api-v2` to RFC 9457 problem details with a real status. Nothing in
//! this crate knows about HTTP, which is what stops one surface's quirks from
//! leaking into the other.
//!
//! Variants carry enough structure for both mappings: `SizeLimit` keeps the
//! numbers so v2 can say which limit was hit, where legacy only needs to know
//! it was a 403.

use crate::ids::{FileId, IdEncoded};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    // --- authentication and authorisation ---
    #[error("no API key provided")]
    MissingApiKey,
    #[error("invalid API key")]
    InvalidApiKey,
    #[error("API key revoked")]
    RevokedApiKey,
    #[error("authentication required")]
    Unauthenticated,
    #[error("forbidden")]
    Forbidden,
    #[error("role not permitted: {0}")]
    RoleNotPermitted(&'static str),
    /// Wrong login or password. Deliberately one variant for both, so a
    /// response never says which accounts exist.
    #[error("invalid login or password")]
    InvalidCredentials,
    /// Too many failed logins; the account accepts none until the lockout
    /// passes.
    #[error("account locked; try again in {retry_after_secs}s")]
    AccountLocked { retry_after_secs: u64 },
    /// The ALTCHA proof-of-work was missing, wrong, expired or already used.
    #[error("human verification failed")]
    HumanVerificationFailed,
    #[error("registration is closed")]
    RegistrationClosed,
    /// A password album, not unlocked by this caller. Its existence is no
    /// secret to whoever has the link; its contents are.
    #[error("this album needs its password")]
    AlbumLocked,
    /// A Pro feature, and this instance has no licence for it: audio
    /// uploads, external storage. Names the feature.
    #[error("{0} needs Haloid Pro")]
    ProRequired(&'static str),

    // --- request shape ---
    #[error("unsupported API version")]
    UnsupportedApiVersion,
    #[error("unknown action")]
    UnknownAction,
    #[error("invalid base64 source")]
    InvalidBase64,
    /// The legacy API rejects base64 sources on GET specifically, with its own
    /// code.
    #[error("base64 sources must be sent by POST")]
    Base64ViaGet,
    #[error("empty upload source")]
    EmptySource,
    #[error("invalid parameter {field}: {reason}")]
    InvalidParam { field: String, reason: String },
    /// Several fields at once, for v2's `errors{field: [...]}`.
    #[error("{} invalid parameters", .0.len())]
    InvalidParams(Vec<FieldError>),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("conflict: {0}")]
    Conflict(String),

    // --- policy ---
    #[error("extension not allowed: {0}")]
    ExtensionNotAllowed(String),
    #[error("file too large: {size} bytes exceeds the {limit} byte limit")]
    SizeLimit { size: u64, limit: u64 },
    #[error("image too large: {pixels} pixels exceeds the {limit} pixel limit")]
    PixelLimit { pixels: u64, limit: u64 },
    #[error("stop word detected")]
    StopWord,
    #[error("duplicate upload")]
    Duplicate { existing: Box<FileRef> },
    #[error("rate limited")]
    RateLimited { retry_after_secs: u64 },

    // --- infrastructure ---
    #[error("media error")]
    Media(#[from] MediaError),
    #[error("storage error")]
    Storage(#[from] StorageError),
    #[error("no storage has capacity for this upload")]
    StorageFull,
    #[error("database error")]
    Database(#[source] BoxError),
    #[error("url fetch rejected: {0}")]
    UrlFetchRejected(String),
    #[error("internal error")]
    Internal(#[source] BoxError),
}

/// One field's problem, in [`DomainError::InvalidParams`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: String,
    pub reason: String,
}

/// Identifies an existing file without dragging the whole record along —
/// enough for a duplicate response to point at what already exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    pub id: FileId,
    pub id_encoded: IdEncoded,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// libvips or ffprobe could not read the file. Maps to the legacy API's
    /// code 600, which is literally named after ffprobe, so the mapping has to
    /// be explicit rather than incidental.
    #[error("could not probe media: {0}")]
    ProbeFailed(String),
    #[error("unsupported media type: {0}")]
    Unsupported(String),
    #[error("derivative generation failed: {0}")]
    DeriveFailed(String),
    #[error("media tool timed out after {secs}s")]
    Timeout { secs: u64 },
    #[error("media tool unavailable: {0}")]
    ToolUnavailable(&'static str),
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("object not found: {0}")]
    NotFound(String),
    #[error("storage rejected the request: {0}")]
    Rejected(String),
    #[error("storage is unreachable: {0}")]
    Unreachable(String),
    #[error("invalid storage key: {0}")]
    InvalidKey(String),
    /// A storage's configuration cannot work: an unparseable config, a
    /// quarantine inside the served directory, a kind not yet supported.
    #[error("storage misconfigured: {0}")]
    Misconfigured(String),
    #[error("storage i/o error")]
    Io(#[from] std::io::Error),
}

impl DomainError {
    /// Convenience for the common invalid-parameter case.
    pub fn invalid_param(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidParam {
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// Whether this error's `Display` text is safe to show a client verbatim.
    ///
    /// Infrastructure failures are not: their messages carry connection
    /// strings, file paths and driver internals. Those are logged with the
    /// request id and the client gets a generic message instead.
    pub fn is_client_safe(&self) -> bool {
        !matches!(
            self,
            Self::Database(_) | Self::Internal(_) | Self::Storage(_) | Self::Media(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infrastructure_errors_are_not_client_safe() {
        let io = std::io::Error::other("connect to mysql://user:hunter2@host failed");
        assert!(!DomainError::Database(Box::new(io)).is_client_safe());
        assert!(!DomainError::Internal(Box::new(std::io::Error::other("x"))).is_client_safe());
        assert!(
            !DomainError::Media(MediaError::ProbeFailed("/tmp/upload-x1y2 broken".into()))
                .is_client_safe()
        );
        assert!(!DomainError::Storage(StorageError::Unreachable("bucket".into())).is_client_safe());
    }

    #[test]
    fn policy_and_request_errors_are_client_safe() {
        for error in [
            DomainError::MissingApiKey,
            DomainError::InvalidApiKey,
            DomainError::EmptySource,
            DomainError::Base64ViaGet,
            DomainError::Forbidden,
            DomainError::StopWord,
            DomainError::StorageFull,
            DomainError::InvalidCredentials,
            DomainError::AccountLocked {
                retry_after_secs: 60,
            },
            DomainError::HumanVerificationFailed,
            DomainError::ExtensionNotAllowed("svg".into()),
            DomainError::SizeLimit {
                size: 100,
                limit: 10,
            },
            DomainError::invalid_param("expiration", "not a duration"),
            DomainError::InvalidParams(vec![FieldError {
                field: "site.name".into(),
                reason: "too long".into(),
            }]),
        ] {
            assert!(error.is_client_safe(), "{error:?} should be client safe");
        }
    }

    #[test]
    fn size_limit_message_names_both_numbers() {
        let error = DomainError::SizeLimit {
            size: 20_000_000,
            limit: 10_000_000,
        };
        let text = error.to_string();
        assert!(text.contains("20000000"), "{text}");
        assert!(text.contains("10000000"), "{text}");
    }
}
