//! haloid-service — the service layer.
//!
//! The only caller of the upload pipeline, and the only way the API crates
//! reach the database, storage and media layers.

pub mod admin;
pub mod album_locks;
pub mod app;
pub mod auth;
pub mod chunked;
pub mod edit;
pub mod fetch;
pub mod files;
pub mod library;
pub mod licensing;
pub mod metrics;
pub mod rescue;
pub mod settings;
pub mod storages;
pub mod updates;
pub mod upload;
