//! haloid-storage — where uploaded bytes live.
//!
//! Implements `haloid_core::storage::Storage` for the local filesystem,
//! builds and validates storage keys, and holds the pool of configured
//! storages. Other kinds of storage (S3 and the S3-compatible services) are
//! Haloid Pro, and plug in through [`backend::StorageBackend`].

pub mod backend;
pub mod keys;
pub mod local;
pub mod pool;
