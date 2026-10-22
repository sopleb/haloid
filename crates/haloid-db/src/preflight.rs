//! Startup checks that fail loudly rather than mysteriously.

use crate::Db;

#[derive(Debug, thiserror::Error)]
pub enum PreflightError {
    #[error(
        "MariaDB's innodb_default_row_format is {found:?}, but Haloid needs \
         \"dynamic\". With a compact or redundant row format, utf8mb4 unique \
         indexes exceed InnoDB's 767-byte key prefix and the migrations will \
         fail. Set innodb_default_row_format=dynamic and restart the server."
    )]
    RowFormat { found: String },

    #[error(
        "MariaDB reports version {found:?}; Haloid needs 10.6 or newer for \
         SELECT ... FOR UPDATE SKIP LOCKED, which the job queue uses to let \
         two processes share a queue safely."
    )]
    ServerVersion { found: String },

    #[error("could not run startup checks")]
    Query(#[from] sqlx::Error),
}

/// Verify the database is configured the way the schema and the job queue
/// assume. Called once at startup, before migrations.
pub async fn preflight(db: &Db) -> Result<(), PreflightError> {
    let (version, row_format): (String, String) =
        sqlx::query_as("SELECT VERSION(), @@innodb_default_row_format")
            .fetch_one(db)
            .await?;

    if !row_format.eq_ignore_ascii_case("dynamic") {
        return Err(PreflightError::RowFormat { found: row_format });
    }
    if !version_is_supported(&version) {
        return Err(PreflightError::ServerVersion { found: version });
    }
    Ok(())
}

/// Whether a reported server version is at least MariaDB 10.6.
///
/// Version strings carry distribution noise (`12.3.3-MariaDB-ubu2404`), so
/// only the leading numeric components are read; anything unparseable is
/// accepted rather than blocking startup on a string we failed to understand.
fn version_is_supported(version: &str) -> bool {
    let numeric: String = version
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = numeric.split('.').filter_map(|p| p.parse::<u32>().ok());
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) => (major, minor) >= (10, 6),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::version_is_supported;

    #[test]
    fn accepts_supported_mariadb_versions() {
        for version in [
            "12.3.3-MariaDB-ubu2404",
            "11.4.12-MariaDB",
            "10.11.6-MariaDB-log",
            "10.6.0-MariaDB",
        ] {
            assert!(version_is_supported(version), "{version} was rejected");
        }
    }

    #[test]
    fn rejects_versions_without_skip_locked() {
        for version in ["10.5.23-MariaDB", "10.3.39-MariaDB", "5.5.68-MariaDB"] {
            assert!(!version_is_supported(version), "{version} was accepted");
        }
    }

    #[test]
    fn an_unparseable_version_does_not_block_startup() {
        // Better to run and let a real query fail than to refuse to boot over
        // a version string we did not anticipate.
        for version in ["", "MariaDB", "unknown"] {
            assert!(version_is_supported(version), "{version:?} blocked startup");
        }
    }
}
