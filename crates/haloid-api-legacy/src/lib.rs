//! haloid-api-legacy — `POST /api/1/upload`, reproduced bug for bug.
//!
//! The compatibility surface every existing upstream, ShareX and ImgBB client
//! talks to. The weird parts are the specification: each carries a comment
//! saying so, and `fixtures/golden/upstream/` holds the real responses they
//! were checked against. This crate depends on `haloid-service` and
//! `haloid-core` only (see `CLAUDE.md`), builds the same `UploadCommand` as
//! the v2 API, and maps `DomainError` to upstream's codes in [`errors`].

pub mod errors;
pub mod handler;
pub mod php_json;
pub mod request;
pub mod response;

pub use handler::router;
