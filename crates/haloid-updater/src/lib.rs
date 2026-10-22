//! The updater sidecar: replaces a Docker install's Haloid container with a
//! newer signed release.
//!
//! It runs as its own small service beside Haloid and is the only thing
//! that holds the Docker socket. Haloid asks it, over a private network and
//! with a shared token, to move to a release; the sidecar trusts none of
//! that request but the token. It checks the release's signature itself,
//! asks the running container what it is (`haloid-server --version`) and
//! refuses anything that is not newer and of the same edition, so a
//! compromised Haloid cannot downgrade itself to an older signed release.
//!
//! Then: pull the image by digest; tag it under the name the container was
//! started from, so `docker compose up` later keeps it; stop the container,
//! keep it aside as `<name>-previous`, create its replacement with the same
//! configuration on the new image, start it, and wait for its health check.
//! If it never becomes healthy the old container is put back. Settings the
//! old image supplied (its environment, command, labels…) are not copied,
//! so the new image's own apply.

use bollard::Docker;
use bollard::auth::DockerCredentials;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, HealthStatusEnum, ImageConfig, NetworkingConfig,
};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, InspectContainerOptions, ListContainersOptions,
    RemoveContainerOptions, RenameContainerOptions, StartContainerOptions, StopContainerOptions,
    TagImageOptions,
};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use haloid_release::{Edition, ImageRelease};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The label that marks the container to update, when
/// `HALOID_UPDATER_CONTAINER` does not name it.
pub const TARGET_LABEL: &str = "so.plb.haloid.updates";

pub struct Config {
    /// What Haloid must present: `Authorization: Bearer <token>`.
    pub token: String,
    /// The container to update, by name; otherwise the one container
    /// labelled [`TARGET_LABEL`]`=server`.
    pub container: Option<String>,
    /// Public keys an image release may be signed with.
    pub trusted: Vec<[u8; 32]>,
    /// How long the new container has to become healthy.
    pub health_timeout: Duration,
}

/// What Haloid sends.
#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    /// The signed release, `HALOIDIMG1.…`.
    pub release: String,
    /// Credentials for a private registry (Pro), handed out with the release.
    pub registry_auth: Option<RegistryAuth>,
}

#[derive(Debug, Deserialize)]
pub struct RegistryAuth {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Idle,
    Pulling,
    Replacing,
    /// The new container is up; waiting for its health check.
    Waiting,
    Done,
    /// It did not work, and the old container is back.
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub phase: Phase,
    /// The version being, or last, installed.
    pub version: Option<String>,
    /// What went wrong, for `failed`.
    pub detail: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("{0}")]
    Refused(String),
    #[error("an update is already under way")]
    Busy,
    #[error("docker: {0}")]
    Docker(#[from] bollard::errors::Error),
}

pub struct Updater {
    docker: Docker,
    config: Config,
    status: Mutex<Status>,
}

impl Updater {
    pub fn new(docker: Docker, config: Config) -> Arc<Self> {
        Arc::new(Self {
            docker,
            config,
            status: Mutex::new(Status {
                phase: Phase::Idle,
                version: None,
                detail: None,
                finished_at: None,
            }),
        })
    }

    pub fn token(&self) -> &str {
        &self.config.token
    }

    pub fn status(&self) -> Status {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set(&self, phase: Phase) {
        self.lock().phase = phase;
    }

    /// Check the request and start the update. Returns the version once
    /// it has started; [`Updater::status`] follows it.
    pub async fn start(self: &Arc<Self>, request: UpdateRequest) -> Result<String, UpdateError> {
        let release = haloid_release::verify_image(&request.release, &self.config.trusted)
            .map_err(|e| UpdateError::Refused(e.to_string()))?;
        {
            let mut status = self.lock();
            if matches!(
                status.phase,
                Phase::Pulling | Phase::Replacing | Phase::Waiting
            ) {
                return Err(UpdateError::Busy);
            }
            status.phase = Phase::Pulling;
            status.version = Some(release.version.clone());
            status.detail = None;
            status.finished_at = None;
        }
        let checked = self.check_target(&release).await;
        let target = match checked {
            Ok(target) => target,
            Err(error) => {
                self.set(Phase::Idle);
                return Err(error);
            }
        };
        let version = release.version.clone();
        let this = self.clone();
        tokio::spawn(async move {
            let outcome = this.update(&target, &release, request.registry_auth).await;
            let mut status = this.lock();
            status.finished_at = Some(Utc::now());
            match outcome {
                Ok(()) => {
                    tracing::info!(version = %release.version, "updated");
                    status.phase = Phase::Done;
                }
                Err(error) => {
                    tracing::error!(%error, version = %release.version, "the update failed");
                    status.phase = Phase::Failed;
                    status.detail = Some(error);
                }
            }
        });
        Ok(version)
    }

    /// The container to update, and a check that the release is newer than
    /// what it runs and of the same edition: asked of the container, not
    /// taken from the request.
    async fn check_target(&self, release: &ImageRelease) -> Result<String, UpdateError> {
        let target = self.find_target().await?;
        let said = self
            .exec(&target, &["haloid-server", "--version"])
            .await
            .map_err(|e| UpdateError::Refused(format!("asking {target} its version: {e}")))?;
        let (version, edition) = parse_version(&said).ok_or_else(|| {
            UpdateError::Refused(format!("{target} did not say what it is: {said:?}"))
        })?;
        release
            .applies(edition, &version)
            .map_err(|e| UpdateError::Refused(format!("{target} runs {version}: {e}")))?;
        Ok(target)
    }

    async fn find_target(&self) -> Result<String, UpdateError> {
        if let Some(name) = &self.config.container {
            return Ok(name.clone());
        }
        let filters =
            HashMap::from([("label".to_string(), vec![format!("{TARGET_LABEL}=server")])]);
        let found = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await?;
        match found.as_slice() {
            [one] => Ok(one
                .names
                .as_ref()
                .and_then(|n| n.first())
                .map(|n| n.trim_start_matches('/').to_string())
                .or_else(|| one.id.clone())
                .unwrap_or_default()),
            [] => Err(UpdateError::Refused(format!(
                "no container is labelled {TARGET_LABEL}=server"
            ))),
            _ => Err(UpdateError::Refused(format!(
                "more than one container is labelled {TARGET_LABEL}=server; set HALOID_UPDATER_CONTAINER"
            ))),
        }
    }

    async fn exec(&self, container: &str, cmd: &[&str]) -> Result<String, bollard::errors::Error> {
        let exec = self
            .docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(cmd.iter().map(|s| (*s).to_string()).collect()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(false),
                    ..Default::default()
                },
            )
            .await?;
        let mut out = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            self.docker.start_exec(&exec.id, None).await?
        {
            while let Some(chunk) = output.next().await {
                out.push_str(&chunk?.to_string());
            }
        }
        Ok(out)
    }

    async fn update(
        &self,
        target: &str,
        release: &ImageRelease,
        auth: Option<RegistryAuth>,
    ) -> Result<(), String> {
        // 1. The image, by digest.
        let credentials = auth.map(|a| DockerCredentials {
            username: Some(a.username),
            password: Some(a.password),
            serveraddress: release.repository().split('/').next().map(str::to_string),
            ..Default::default()
        });
        let mut pull = self.docker.create_image(
            Some(CreateImageOptions {
                from_image: Some(release.image.clone()),
                ..Default::default()
            }),
            None,
            credentials,
        );
        while let Some(step) = pull.next().await {
            step.map_err(|e| format!("pulling {}: {e}", release.image))?;
        }

        let old = self
            .docker
            .inspect_container(target, None::<InspectContainerOptions>)
            .await
            .map_err(|e| format!("inspecting {target}: {e}"))?;
        let started_from = old
            .config
            .as_ref()
            .and_then(|c| c.image.clone())
            .unwrap_or_default();

        // 2. Under the name it was started from, so compose keeps it.
        if let Some((repo, tag)) = tag_of(&started_from) {
            self.docker
                .tag_image(
                    &release.image,
                    Some(TagImageOptions {
                        repo: Some(repo.to_string()),
                        tag: Some(tag.to_string()),
                    }),
                )
                .await
                .map_err(|e| format!("tagging {started_from}: {e}"))?;
        }

        let old_image = match &old.image {
            Some(id) => self
                .docker
                .inspect_image(id)
                .await
                .ok()
                .and_then(|i| i.config),
            None => None,
        };
        let new_image_ref = if tag_of(&started_from).is_some() {
            started_from.clone()
        } else {
            release.image.clone()
        };
        let body = replacement(&old, old_image.as_ref(), &new_image_ref);

        // 3. Swap the containers.
        self.set(Phase::Replacing);
        let name = old
            .name
            .as_deref()
            .unwrap_or(target)
            .trim_start_matches('/')
            .to_string();
        let aside = format!("{name}-previous");
        let _ = self
            .docker
            .remove_container(
                &aside,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        self.docker
            .stop_container(
                &name,
                Some(StopContainerOptions {
                    t: Some(60),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| format!("stopping {name}: {e}"))?;
        self.docker
            .rename_container(
                &name,
                RenameContainerOptions {
                    name: aside.clone(),
                },
            )
            .await
            .map_err(|e| format!("setting {name} aside: {e}"))?;

        let started = self.start_replacement(&name, body).await;
        let healthy = match started {
            Ok(()) => {
                self.set(Phase::Waiting);
                self.wait_healthy(&name).await
            }
            Err(error) => Err(error),
        };
        match healthy {
            Ok(()) => {
                let _ = self
                    .docker
                    .remove_container(&aside, None::<RemoveContainerOptions>)
                    .await;
                Ok(())
            }
            Err(error) => {
                // The name the container was started from goes back to the
                // old image too, or the next `docker compose up` would bring
                // the failed one back.
                if let (Some((repo, tag)), Some(old_id)) = (tag_of(&started_from), &old.image)
                    && let Err(e) = self
                        .docker
                        .tag_image(
                            old_id,
                            Some(TagImageOptions {
                                repo: Some(repo.to_string()),
                                tag: Some(tag.to_string()),
                            }),
                        )
                        .await
                {
                    tracing::error!(error = %e, "could not point {started_from} back at the previous image");
                }
                let restored = self.put_back(&name, &aside).await;
                Err(match restored {
                    Ok(()) => format!(
                        "{error}; the previous container is running again (if the new version had already changed the database, it may refuse to start: restore the backup)"
                    ),
                    Err(also) => {
                        format!("{error}; putting the previous container back failed too: {also}")
                    }
                })
            }
        }
    }

    async fn start_replacement(&self, name: &str, body: ContainerCreateBody) -> Result<(), String> {
        let extra: Vec<(String, bollard::models::EndpointSettings)> = body
            .networking_config
            .as_ref()
            .and_then(|n| n.endpoints_config.clone())
            .map(|m| m.into_iter().skip(1).collect())
            .unwrap_or_default();
        let mut first = body;
        // Created on one network, then connected to the rest: older
        // engines accept only one at creation.
        if let Some(net) = first.networking_config.as_mut()
            && let Some(endpoints) = net.endpoints_config.as_mut()
        {
            let keep: Option<String> = endpoints.keys().next().cloned();
            endpoints.retain(|k, _| Some(k) == keep.as_ref());
        }
        self.docker
            .create_container(
                Some(CreateContainerOptions {
                    name: Some(name.to_string()),
                    ..Default::default()
                }),
                first,
            )
            .await
            .map_err(|e| format!("creating the new {name}: {e}"))?;
        for (network, endpoint) in extra {
            self.docker
                .connect_network(
                    &network,
                    bollard::models::NetworkConnectRequest {
                        container: name.to_string(),
                        endpoint_config: Some(endpoint),
                    },
                )
                .await
                .map_err(|e| format!("connecting the new {name} to {network}: {e}"))?;
        }
        self.docker
            .start_container(name, None::<StartContainerOptions>)
            .await
            .map_err(|e| format!("starting the new {name}: {e}"))
    }

    /// Healthy by its health check, or, for an image without one, still
    /// running after half a minute.
    async fn wait_healthy(&self, name: &str) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + self.config.health_timeout;
        let started = tokio::time::Instant::now();
        loop {
            let state = self
                .docker
                .inspect_container(name, None::<InspectContainerOptions>)
                .await
                .map_err(|e| e.to_string())?
                .state
                .unwrap_or_default();
            if state.running != Some(true) {
                return Err(format!("the new {name} stopped"));
            }
            match state.health.and_then(|h| h.status) {
                Some(HealthStatusEnum::HEALTHY) => return Ok(()),
                Some(HealthStatusEnum::UNHEALTHY) => {
                    return Err(format!("the new {name} is unhealthy"));
                }
                Some(HealthStatusEnum::STARTING) => {}
                _ if started.elapsed() >= Duration::from_secs(30) => return Ok(()),
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "the new {name} was not healthy within {}s",
                    self.config.health_timeout.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    async fn put_back(&self, name: &str, aside: &str) -> Result<(), String> {
        let _ = self
            .docker
            .remove_container(
                name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        self.docker
            .rename_container(
                aside,
                RenameContainerOptions {
                    name: name.to_string(),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        self.docker
            .start_container(name, None::<StartContainerOptions>)
            .await
            .map_err(|e| e.to_string())
    }
}

/// `haloid-server 0.2.0 community x86_64-…` → (version, edition).
pub fn parse_version(said: &str) -> Option<(String, Edition)> {
    let line = said.lines().find(|l| l.starts_with("haloid-server "))?;
    let mut words = line.split_whitespace().skip(1);
    let version = words.next()?.to_string();
    let edition = match words.next()? {
        "community" => Edition::Community,
        "pro" => Edition::Pro,
        _ => return None,
    };
    Some((version, edition))
}

/// `registry/haloid:stable` → (`registry/haloid`, `stable`); `None` for an
/// image named by digest or by id, which has no name to keep.
pub fn tag_of(image: &str) -> Option<(&str, &str)> {
    if image.contains('@') || image.starts_with("sha256:") || image.is_empty() {
        return None;
    }
    // A colon after the last slash is a tag; one before it is a port.
    let slash = image.rfind('/').map_or(0, |i| i + 1);
    match image[slash..].rfind(':') {
        Some(colon) => Some((&image[..slash + colon], &image[slash + colon + 1..])),
        None => Some((image, "latest")),
    }
}

/// The new container: the old one's configuration on `image`, without what
/// the old image supplied, so the new image's defaults apply.
pub fn replacement(
    old: &ContainerInspectResponse,
    old_image: Option<&ImageConfig>,
    image: &str,
) -> ContainerCreateBody {
    let config = old.config.clone().unwrap_or_default();
    // The two share Docker's field names; going through JSON keeps every
    // field without listing them.
    let mut body: ContainerCreateBody = serde_json::to_value(&config)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    body.image = Some(image.to_string());
    if let Some(from) = old_image {
        if let (Some(env), Some(defaults)) = (&body.env, &from.env) {
            let kept: Vec<String> = env
                .iter()
                .filter(|e| !defaults.contains(e))
                .cloned()
                .collect();
            body.env = Some(kept);
        }
        if let (Some(labels), Some(defaults)) = (&body.labels, &from.labels) {
            let kept: HashMap<String, String> = labels
                .iter()
                .filter(|(k, v)| defaults.get(*k) != Some(*v))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            body.labels = Some(kept);
        }
        if body.cmd == from.cmd {
            body.cmd = None;
        }
        if body.entrypoint == from.entrypoint {
            body.entrypoint = None;
        }
        if body.working_dir == from.working_dir {
            body.working_dir = None;
        }
        if body.user == from.user {
            body.user = None;
        }
        if body.stop_signal == from.stop_signal {
            body.stop_signal = None;
        }
        if body.healthcheck == from.healthcheck {
            body.healthcheck = None;
        }
        if body.volumes == from.volumes {
            body.volumes = None;
        }
        if body.exposed_ports == from.exposed_ports {
            body.exposed_ports = None;
        }
    }
    // A hostname Docker made up from the old container's id.
    if let (Some(host), Some(id)) = (&body.hostname, &old.id)
        && id.starts_with(host.as_str())
    {
        body.hostname = None;
    }
    body.host_config = old.host_config.clone();
    body.networking_config = old
        .network_settings
        .as_ref()
        .and_then(|n| n.networks.clone())
        .map(|networks| NetworkingConfig {
            endpoints_config: Some(
                networks
                    .into_iter()
                    .map(|(name, mut endpoint)| {
                        // What Docker assigned the old container; the new
                        // one gets its own.
                        endpoint.endpoint_id = None;
                        endpoint.network_id = None;
                        endpoint.ip_address = None;
                        endpoint.gateway = None;
                        endpoint.mac_address = None;
                        endpoint.global_ipv6_address = None;
                        endpoint.ipv6_gateway = None;
                        endpoint.dns_names = None;
                        (name, endpoint)
                    })
                    .collect(),
            ),
        });
    body
}

pub mod http;
