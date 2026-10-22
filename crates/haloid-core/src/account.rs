//! Accounts: users, their login sessions, and their API keys.
//!
//! Secret material is deliberately absent from these types. A password hash,
//! a session token hash or an API key's HMAC key travels separately, in the
//! one lookup that needs it, so that a `User` or an `ApiKey` can be logged or
//! serialised without a second thought.

use crate::DomainError;
use crate::auth::{Actor, Role};
use crate::ids::{ApiKeyId, IdEncoded, SessionId, UserId};
use chrono::{DateTime, Utc};
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

/// Usernames that would collide with a route, or impersonate the site. The
/// public profile lives at `/u/{username}`, so these are refused regardless.
pub const RESERVED_USERNAMES: &[&str] = &[
    "f",
    "a",
    "u",
    "api",
    "login",
    "logout",
    "register",
    "account",
    "admin",
    "assets",
    "explore",
    "search",
    "embed",
    "delete",
    "maintenance",
    "settings",
    "static",
    "haloid",
    "root",
    "system",
    "moderator",
    "support",
    "guest",
    "anonymous",
];

/// Usernames: 3 to 32 ASCII letters, digits, `-` and `_`, starting with a
/// letter or digit, not reserved.
///
/// ASCII only, on purpose: usernames appear in URLs and are compared
/// case-insensitively, and Unicode lookalikes (Cyrillic `а` for Latin `a`)
/// would let one user impersonate another. `@` is excluded too, which is
/// what lets login tell a username from an email address.
pub fn validate_username(username: &str) -> Result<(), DomainError> {
    let invalid = |reason: &str| Err(DomainError::invalid_param("username", reason));
    if !(3..=32).contains(&username.len()) {
        return invalid("must be 3 to 32 characters");
    }
    if !username
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return invalid("may contain only letters, digits, - and _");
    }
    if !username.as_bytes()[0].is_ascii_alphanumeric() {
        return invalid("must start with a letter or digit");
    }
    let lower = username.to_ascii_lowercase();
    if RESERVED_USERNAMES.contains(&lower.as_str()) {
        return invalid("is reserved");
    }
    Ok(())
}

/// A deliberately loose email check: one `@` with something on both sides, a
/// dot in the domain, no whitespace, within the column. Real validation is
/// sending mail to it; a strict regex only rejects real addresses.
pub fn validate_email(email: &str) -> Result<(), DomainError> {
    let invalid = || {
        Err(DomainError::invalid_param(
            "email",
            "is not an email address",
        ))
    };
    if email.len() > 190 || email.chars().any(char::is_whitespace) {
        return invalid();
    }
    match email.split_once('@') {
        Some((local, domain))
            if !local.is_empty()
                && !domain.contains('@')
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.') =>
        {
            Ok(())
        }
        _ => invalid(),
    }
}

/// Passwords: at least 10 characters, at most 1024 bytes. Length, not
/// composition rules, is what resists guessing; the upper bound keeps one
/// request from making argon2 hash megabytes.
pub fn validate_password(password: &str) -> Result<(), DomainError> {
    if password.chars().count() < 10 {
        return Err(DomainError::invalid_param(
            "password",
            "must be at least 10 characters",
        ));
    }
    if password.len() > 1024 {
        return Err(DomainError::invalid_param("password", "is too long"));
    }
    Ok(())
}

/// Whether an account may be used at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserStatus {
    Valid,
    /// Registered but has not confirmed their email address.
    AwaitingConfirmation,
    Banned,
}

impl UserStatus {
    /// The wire and database representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Banned => "banned",
        }
    }

    pub const ALL: [UserStatus; 3] = [Self::Valid, Self::AwaitingConfirmation, Self::Banned];
}

impl fmt::Display for UserStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for UserStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "valid" => Ok(Self::Valid),
            "awaiting_confirmation" => Ok(Self::AwaitingConfirmation),
            "banned" => Ok(Self::Banned),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: UserId,
    pub id_encoded: IdEncoded,
    /// As the user typed it. Uniqueness and lookup are case-insensitive.
    pub username: String,
    pub email: Option<String>,
    pub role: Role,
    pub status: UserStatus,
    pub display_name: Option<String>,
    pub is_private: bool,
    pub file_count: u32,
    pub album_count: u32,
    pub disk_used: u64,
    pub failed_logins: u32,
    pub locked_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl User {
    /// This user as the actor behind a request.
    pub const fn actor(&self) -> Actor {
        Actor::User {
            id: self.id,
            role: self.role,
        }
    }

    /// Whether failed-login lockout is in force at `now`.
    pub fn is_locked(&self, now: DateTime<Utc>) -> bool {
        self.locked_until.is_some_and(|until| until > now)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: SessionId,
    pub user_id: UserId,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Session {
    /// Expiry is exclusive: a session is dead at its `expires_at` instant.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiKey {
    pub id: ApiKeyId,
    /// `None` is an instance-wide guest key, which uploads as a guest.
    pub user_id: Option<UserId>,
    pub label: String,
    /// The public lookup handle at the front of the key, and all we ever
    /// display of it.
    pub key_prefix: String,
    pub is_active: bool,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApiKey {
    pub const fn is_guest_key(&self) -> bool {
        self.user_id.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    fn user() -> User {
        User {
            id: UserId(7),
            id_encoded: "abcdefghij".into(),
            username: "Alice".into(),
            email: None,
            role: Role::Manager,
            status: UserStatus::Valid,
            display_name: None,
            is_private: false,
            file_count: 0,
            album_count: 0,
            disk_used: 0,
            failed_logins: 0,
            locked_until: None,
            created_at: at(0),
            last_login_at: None,
        }
    }

    #[test]
    fn usernames_are_plain_and_unreserved() {
        for good in ["alice", "Bob_99", "a-b", "x2y", &"a".repeat(32)] {
            assert!(validate_username(good).is_ok(), "{good:?} rejected");
        }
        for bad in [
            "",
            "ab",
            &"a".repeat(33),
            "has space",
            "dot.ted",
            "at@sign",
            "_leading",
            "-leading",
            "аlice", // Cyrillic а
            "admin",
            "ADMIN",
            "api",
            "u",
        ] {
            assert!(validate_username(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn emails_are_checked_loosely() {
        for good in ["a@b.co", "first.last+tag@example.co.uk", "ü@例え.jp"] {
            assert!(validate_email(good).is_ok(), "{good:?} rejected");
        }
        for bad in [
            "",
            "plain",
            "@example.com",
            "a@",
            "a@b",
            "a@@b.com",
            "a b@c.com",
            "a@.com",
            "a@b.",
        ] {
            assert!(validate_email(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn passwords_need_length_not_rules() {
        assert!(validate_password("correct horse").is_ok());
        assert!(validate_password("short").is_err());
        assert!(validate_password("ninechars").is_err());
        assert!(
            validate_password("éééééééééé").is_ok(),
            "counted in characters"
        );
        assert!(validate_password(&"a".repeat(1025)).is_err());
    }

    #[test]
    fn status_wire_names_round_trip() {
        for status in UserStatus::ALL {
            assert_eq!(status.as_str().parse::<UserStatus>(), Ok(status));
        }
        for input in ["", "Valid", "awaiting", "deleted"] {
            assert!(input.parse::<UserStatus>().is_err(), "{input:?} parsed");
        }
    }

    #[test]
    fn a_user_acts_with_its_own_id_and_role() {
        let actor = user().actor();
        assert_eq!(actor.user_id(), Some(UserId(7)));
        assert_eq!(actor.role(), Some(Role::Manager));
    }

    #[test]
    fn lockout_ends_at_its_deadline() {
        let mut u = user();
        assert!(!u.is_locked(at(100)), "never locked");

        u.locked_until = Some(at(100));
        assert!(u.is_locked(at(99)));
        assert!(!u.is_locked(at(100)), "lockout is over at its deadline");
        assert!(!u.is_locked(at(101)));
    }

    #[test]
    fn a_session_is_dead_at_its_expiry_instant() {
        let session = Session {
            id: SessionId("s".into()),
            user_id: UserId(1),
            ip: None,
            user_agent: None,
            created_at: at(0),
            last_seen_at: at(0),
            expires_at: at(100),
        };
        assert!(!session.is_expired(at(99)));
        assert!(session.is_expired(at(100)));
    }
}
