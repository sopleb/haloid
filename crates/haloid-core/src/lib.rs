//! haloid-core — the domain.
//!
//! This crate is I/O-free by rule: no axum, no sqlx, no reqwest. It owns the
//! domain types, the [`DomainError`] enum every layer speaks, and the
//! [`Storage`](storage::Storage) / [`MediaProcessor`](processor::MediaProcessor)
//! traits that the infrastructure crates implement.
//!
//! It deliberately implements no `IntoResponse`: each API crate maps
//! [`DomainError`] to its own wire format inside its own crate, which is what
//! keeps the legacy and v2 surfaces from leaking into each other.

pub mod account;
pub mod auth;
pub mod entitlements;
pub mod error;
pub mod expiration;
pub mod files;
pub mod ids;
pub mod media;
pub mod pagination;
pub mod processor;
pub mod settings;
pub mod storage;
pub mod taxonomy;

pub use error::{DomainError, FieldError, MediaError, StorageError};
