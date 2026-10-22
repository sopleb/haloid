//! haloid-api-v2 — our own REST API.
//!
//! Proper status codes, RFC 9457 problem details, real JSON types, RFC 3339
//! dates. Depends on `haloid-service` and `haloid-core` only (see
//! `CLAUDE.md`), building the same `UploadCommand` as the legacy surface.

pub mod account_routes;
pub mod admin_routes;
pub mod auth_routes;
pub mod caller;
pub mod library_routes;
pub mod openapi;
pub mod problem;
pub mod routes;
pub mod session_routes;
pub mod views;
pub mod write_routes;

pub use routes::router;
