//! haloid-server — the single binary's wiring, as a library so it can be
//! tested: bootstrap configuration and the HTTP router.

pub mod config;
pub mod head;
pub mod proxy;
pub mod router;
pub mod run;

pub use run::{Build, run};
