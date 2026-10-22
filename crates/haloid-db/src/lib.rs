//! haloid-db — persistence.
//!
//! Owns the connection pool, the embedded migrations, and the repositories.
//! Nothing above the service layer may depend on this crate: see the crate
//! boundary rule in `CLAUDE.md`.
//!
//! Repositories are free functions over [`Db`], one module per aggregate, and
//! they do the row-to-domain mapping so that nothing above them sees a row.

pub mod albums;
pub mod api_keys;
pub mod audit;
pub mod categories;
pub mod error;
pub mod files;
pub mod instance;
mod ip;
pub mod jobs;
pub mod moderation;
pub mod pool;
pub mod preflight;
pub mod sessions;
pub mod settings;
pub mod storages;
pub mod tags;
pub mod upload_sessions;
pub mod users;

pub use error::DbError;
pub use pool::{Db, connect, migrate, ping};
pub use preflight::{PreflightError, preflight};

/// The embedded migration set, so a release binary needs no migration files
/// beside it.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Parse an `ENUM` column into its domain type.
///
/// The schema constrains these columns, so a failure means the schema and
/// the code disagree. That is a decode error for the whole row rather than a
/// silent default.
pub(crate) fn decode_enum<T: std::str::FromStr>(
    column: &str,
    value: &str,
) -> Result<T, sqlx::Error> {
    value
        .parse()
        .map_err(|_| decode_error(column, &format!("unexpected value {value:?}")))
}

/// A row that the schema should have made impossible.
pub(crate) fn decode_error(column: &str, reason: &str) -> sqlx::Error {
    sqlx::Error::ColumnDecode {
        index: column.to_string(),
        source: format!("{column}: {reason}").into(),
    }
}
