//! Repository errors.
//!
//! Most repository functions return a bare `sqlx::Error`: nothing about a
//! failed read is the caller's to act on. Writes guarded by a unique index
//! return [`DbError`] instead, because "that username is taken" is an answer
//! the user needs and a driver error is not.

use haloid_core::DomainError;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A unique index rejected the write. Names the field, never the index,
    /// so it can be shown to a client as is.
    #[error("{0} is already taken")]
    Conflict(&'static str),
    /// The write would break a structural rule the schema cannot express —
    /// an album cycle, a tree too deep. Client-safe, like `Conflict`.
    #[error("invalid {field}: {reason}")]
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
    #[error("database error")]
    Sqlx(#[from] sqlx::Error),
}

impl From<DbError> for DomainError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Conflict(field) => DomainError::Conflict(format!("{field} is already taken")),
            DbError::Invalid { field, reason } => DomainError::invalid_param(field, reason),
            DbError::Sqlx(e) => DomainError::Database(Box::new(e)),
        }
    }
}

/// The name of the unique index a failed write collided with, if that is
/// what happened.
pub(crate) fn unique_violation(error: &sqlx::Error) -> Option<String> {
    let db = error.as_database_error()?;
    if !db.is_unique_violation() {
        return None;
    }
    index_from_message(db.message())
}

/// MariaDB reports `Duplicate entry 'x' for key 'uq_users_username'`; MySQL 8
/// qualifies it as `'ih_users.uq_users_username'`. Both reduce to the index.
fn index_from_message(message: &str) -> Option<String> {
    let quoted = message.rsplit_once("for key '")?.1;
    let key = quoted.strip_suffix('\'').unwrap_or(quoted);
    let key = key.rsplit_once('.').map_or(key, |(_, index)| index);
    Some(key.to_string())
}

#[cfg(test)]
mod tests {
    use super::index_from_message;

    #[test]
    fn reads_the_index_from_both_server_dialects() {
        assert_eq!(
            index_from_message("Duplicate entry 'alice' for key 'uq_users_username'").as_deref(),
            Some("uq_users_username")
        );
        assert_eq!(
            index_from_message("Duplicate entry 'alice' for key 'ih_users.uq_users_username'")
                .as_deref(),
            Some("uq_users_username")
        );
    }

    #[test]
    fn a_value_containing_the_marker_does_not_fool_it() {
        // The duplicate value is user input and comes first; the index is
        // always the last quoted name.
        assert_eq!(
            index_from_message("Duplicate entry 'for key 'x'' for key 'uq_users_email'").as_deref(),
            Some("uq_users_email")
        );
    }

    #[test]
    fn an_unrelated_message_yields_nothing() {
        assert_eq!(index_from_message("Deadlock found"), None);
    }
}
