//! haloid-server — the single binary, Community edition.
//!
//! Everything is in the library (`haloid_server::run`); the Pro build has
//! a main of its own that adds its storage kinds.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    haloid_server::run(haloid_server::Build::community()).await
}
