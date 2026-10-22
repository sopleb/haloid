//! Kinds of storage beyond the local filesystem, plugged in at startup.
//!
//! The Community build knows only local storage. The Pro build hands the
//! server a [`StorageBackend`] for each further kind it supports (S3 and
//! the S3-compatible services), so the public crates never contain that
//! code and a storage of a kind this build lacks is simply not loaded.

use haloid_core::StorageError;
use haloid_core::storage::{Storage, StorageKind, StorageRecord};
use std::sync::Arc;

/// Where a storage reads its credentials from: the process environment in
/// production, a map in tests.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The environment variable names for a storage's credentials. Secrets
/// never go in the database; they are looked up by the storage's name.
pub fn credential_vars(name: &str) -> (String, String) {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    (
        format!("HALOID_STORAGE_{name}_ACCESS_KEY"),
        format!("HALOID_STORAGE_{name}_SECRET_KEY"),
    )
}

pub trait StorageBackend: Send + Sync + 'static {
    fn kind(&self) -> StorageKind;

    /// Build a storage from its row, reading credentials through `env`.
    fn build(
        &self,
        record: &StorageRecord,
        env: EnvLookup<'_>,
    ) -> Result<Arc<dyn Storage>, StorageError>;

    /// Check a `config` JSON before it is saved, without credentials, so a
    /// typo is refused in the admin area rather than at the next start.
    fn check_config(&self, config: &str) -> Result<(), String>;
}
