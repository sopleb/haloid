//! `DomainError` as the legacy API reports it: upstream's messages and
//! numeric codes, word for word where upstream has an equivalent.
//!
//! The codes are not HTTP statuses — `error.code` carries 100, 110, 130, 600
//! and friends inside a response that is always HTTP 400 (or 200, for the
//! text formats). Messages for infrastructure failures are generic: the
//! detail is logged, never sent.

use haloid_core::{DomainError, MediaError};

/// Upstream's `format_bytes`: decimal units, one decimal place, no trailing
/// `.0` (PHP prints a whole float without one).
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let units = ["KB", "MB", "GB", "TB", "PB", "EB"];
    let mut multiplier = 1000f64;
    for unit in units {
        if (bytes as f64) < multiplier * 1000.0 {
            let rounded = (bytes as f64 / multiplier * 10.0).round() / 10.0;
            return if rounded.fract() == 0.0 {
                format!("{} {unit}", rounded as u64)
            } else {
                format!("{rounded} {unit}")
            };
        }
        multiplier *= 1000.0;
    }
    format!("{bytes} B")
}

/// The message and code for an error.
pub fn describe(error: &DomainError) -> (String, i64) {
    use DomainError as E;
    let fixed = |message: &str, code: i64| (message.to_string(), code);
    match error {
        E::MissingApiKey => fixed("No key provided", 100),
        // Upstream has no notion of a revoked key; it is simply invalid.
        E::InvalidApiKey | E::RevokedApiKey => fixed("Invalid API key.", 100),
        E::UnsupportedApiVersion => fixed("Invalid API version.", 110),
        E::UnknownAction => fixed("Invalid API action", 120),
        E::Unauthenticated | E::Forbidden | E::RoleNotPermitted(_) => fixed("Forbidden", 403),
        E::EmptySource => fixed("Empty upload source", 130),
        E::Base64ViaGet => fixed(
            "Upload using base64 source must be done using POST method.",
            130,
        ),
        E::InvalidBase64 => fixed("Invalid base64 string", 120),
        E::SizeLimit { limit, .. } => (format!("File too big - max {}", format_bytes(*limit)), 400),
        E::ExtensionNotAllowed(extension) => (format!("Disabled image format ({extension})"), 614),
        // Upstream has no editions; to a legacy client a feature this
        // instance lacks is a format it has disabled.
        E::ProRequired(what) => (format!("Disabled image format ({what})"), 614),
        E::Media(MediaError::Unsupported(_)) => fixed("Can't get target upload source info", 610),
        // ffprobe's own failures are 600, upstream's "FFprobe error"; a still
        // image that will not read is 610, like an unsupported one.
        E::Media(MediaError::ProbeFailed(detail)) if detail.starts_with("ffprobe") => {
            fixed("FFprobe error", 600)
        }
        E::Media(MediaError::ProbeFailed(_)) => fixed("Can't get target upload source info", 610),
        E::PixelLimit { .. } => fixed("Invalid image", 400),
        E::StopWord => fixed("Forbidden content", 400),
        E::RateLimited { retry_after_secs } => (
            format!("Flooding detected. Try again in {retry_after_secs} seconds"),
            130,
        ),
        E::UrlFetchRejected(_) => fixed("Invalid image URL", 622),
        E::InvalidParam { field, reason } => (format!("Invalid {field}: {reason}"), 400),
        E::InvalidParams(fields) => {
            let first = fields
                .first()
                .map_or_else(String::new, |f| format!(": {} {}", f.field, f.reason));
            (format!("Invalid request{first}"), 400)
        }
        E::NotFound(what) => (format!("{} not found", capitalise(what)), 404),
        E::Conflict(_) | E::Duplicate { .. } => fixed("Duplicated upload", 101),
        E::RegistrationClosed
        | E::InvalidCredentials
        | E::AccountLocked { .. }
        | E::AlbumLocked => fixed("Forbidden", 403),
        E::HumanVerificationFailed => fixed("Forbidden", 403),
        E::StorageFull => fixed("No storage available for this upload", 600),
        E::Media(_) | E::Storage(_) | E::Database(_) | E::Internal(_) => {
            fixed("Internal error", 600)
        }
    }
}

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_format_like_upstream() {
        assert_eq!(format_bytes(232), "232 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1000), "1 KB");
        assert_eq!(format_bytes(1324), "1.3 KB");
        assert_eq!(format_bytes(3470), "3.5 KB");
        assert_eq!(format_bytes(16 * 1024 * 1024), "16.8 MB");
        assert_eq!(format_bytes(2_000_000), "2 MB");
        assert_eq!(
            format_bytes(999_999),
            "1000 KB",
            "upstream rounds within the unit"
        );
    }

    #[test]
    fn messages_match_the_captured_ones() {
        // Each pair is taken from fixtures/golden/upstream/.
        assert_eq!(
            describe(&DomainError::MissingApiKey),
            ("No key provided".into(), 100)
        );
        assert_eq!(
            describe(&DomainError::InvalidApiKey),
            ("Invalid API key.".into(), 100)
        );
        assert_eq!(
            describe(&DomainError::UnsupportedApiVersion),
            ("Invalid API version.".into(), 110)
        );
        assert_eq!(
            describe(&DomainError::EmptySource),
            ("Empty upload source".into(), 130)
        );
        assert_eq!(
            describe(&DomainError::InvalidBase64),
            ("Invalid base64 string".into(), 120)
        );
        assert_eq!(
            describe(&DomainError::Media(MediaError::Unsupported("svg".into()))),
            ("Can't get target upload source info".into(), 610)
        );
    }

    #[test]
    fn infrastructure_detail_never_reaches_the_client() {
        let leaky = DomainError::Database("mysql://root:hunter2@db".into());
        let (message, code) = describe(&leaky);
        assert!(!message.contains("hunter2"));
        assert_eq!(code, 600);
    }
}
