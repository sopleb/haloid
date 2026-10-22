//! haloid-updater — the sidecar that updates a Docker install of Haloid.
//!
//! Configuration, from the environment:
//!
//! | Variable | |
//! |---|---|
//! | `HALOID_UPDATER_TOKEN` | Shared with Haloid (its `HALOID_UPDATER_TOKEN`). At least 32 characters. Required |
//! | `HALOID_UPDATER_BIND` | Default `0.0.0.0:8090`; keep it on a network only Haloid shares |
//! | `HALOID_UPDATER_CONTAINER` | The container to update; default: the one labelled `so.plb.haloid.updates=server` |
//! | `HALOID_UPDATER_HEALTH_TIMEOUT` | Seconds the new container has to become healthy. Default 300 |
//!
//! It needs the Docker socket, mounted at `/var/run/docker.sock`.

use haloid_updater::{Config, Updater, http};
use std::process::ExitCode;
use std::time::Duration;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("HALOID_UPDATER_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let token = var("HALOID_UPDATER_TOKEN").ok_or("HALOID_UPDATER_TOKEN is required")?;
    if token.len() < 32 {
        return Err("HALOID_UPDATER_TOKEN must be at least 32 characters".into());
    }
    let health_timeout = var("HALOID_UPDATER_HEALTH_TIMEOUT")
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| "HALOID_UPDATER_HEALTH_TIMEOUT is not a number")
        })
        .transpose()?
        .unwrap_or(300);
    let docker = bollard::Docker::connect_with_local_defaults()
        .map_err(|e| format!("connecting to Docker: {e}"))?;
    docker
        .ping()
        .await
        .map_err(|e| format!("Docker does not answer (is the socket mounted?): {e}"))?;
    let updater = Updater::new(
        docker,
        Config {
            token,
            container: var("HALOID_UPDATER_CONTAINER"),
            trusted: haloid_release::RELEASE_KEYS.to_vec(),
            health_timeout: Duration::from_secs(health_timeout),
        },
    );
    let bind = var("HALOID_UPDATER_BIND").unwrap_or_else(|| "0.0.0.0:8090".into());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("binding {bind}: {e}"))?;
    tracing::info!(address = %bind, "haloid-updater listening");
    axum::serve(listener, http::router(updater))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}
