//! Finding new releases, and installing one in place.
//!
//! Only a binary install updates itself: in a container the image is the
//! unit of upgrade, so the updater is switched off there entirely
//! (`UpdatesConfig::disabled`). Otherwise, depending on `updates.mode`, it
//! looks for a release every [`CHECK_EVERY`], shows administrators what it
//! found, and installs it when one presses Install or, in `auto`, at
//! `updates.hour`.
//!
//! A release is trusted only through its signed manifest (see
//! `haloid-release`): the download must match the manifest's size and
//! SHA-256, and the new binary must say the promised version when run with
//! `--version`, before it replaces the running one. The old binary is kept
//! beside it as `<name>.previous`. The server itself does the restart, when
//! [`Updates::restart_requested`] says so.

use crate::admin::By;
use crate::licensing::Licensing;
use crate::settings::SettingsHandle;
use chrono::{DateTime, Timelike, Utc};
use futures::StreamExt;
use haloid_core::DomainError;
use haloid_core::auth::Actor;
use haloid_core::settings::UpdateMode;
use haloid_db::{Db, audit};
use haloid_release::{Edition, Expect, ImageRelease, Manifest};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;

/// How often to look for a release when updates are on.
pub const CHECK_EVERY: chrono::Duration = chrono::Duration::hours(6);
/// How often the background task wakes to see whether anything is due.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Where releases are announced.
#[derive(Debug, Clone)]
pub enum Source {
    /// A GitLab project's releases API, e.g.
    /// `https://git.plb.so/api/v4/projects/sopleb%2Fhaloid`. The latest
    /// release carries a manifest per edition and target as a link named
    /// `haloid-server-<edition>-<target>.manifest`.
    Gitlab { api: String },
    /// The licence service, which answers only for an active key. `None`
    /// when this instance is not pointed at one.
    LicenceService { server: Option<String> },
}

/// The Community build's release host.
pub const GITLAB_API: &str = "https://git.plb.so/api/v4/projects/sopleb%2Fhaloid";

pub struct UpdatesConfig {
    pub edition: Edition,
    /// The target triple this binary was built for.
    pub target: String,
    /// This binary's version.
    pub current: String,
    /// How a release is put in place.
    pub installer: Installer,
    pub source: Source,
    /// Public keys a release manifest may be signed with.
    pub trusted: Vec<[u8; 32]>,
    /// Why there is no updater at all, e.g. running in a container.
    pub disabled: Option<String>,
}

/// How this install puts a release in place.
#[derive(Debug, Clone)]
pub enum Installer {
    /// Replace the running binary. `None` when it could not be found, and
    /// then nothing can be installed.
    Binary { exe: Option<PathBuf> },
    /// In a container: ask the updater sidecar to replace the container.
    Sidecar { url: String, token: String },
    /// In a container with no sidecar: show the release and how to pull it.
    Container,
}

impl Installer {
    const fn images(&self) -> bool {
        !matches!(self, Self::Binary { .. })
    }
}

impl UpdatesConfig {
    /// No updater: for tests, and anything else that does not describe
    /// itself.
    pub fn none() -> Self {
        Self {
            edition: Edition::Community,
            target: haloid_release::BUILD_TARGET.into(),
            current: env!("CARGO_PKG_VERSION").into(),
            installer: Installer::Binary { exe: None },
            source: Source::Gitlab {
                api: GITLAB_API.into(),
            },
            trusted: haloid_release::RELEASE_KEYS.to_vec(),
            disabled: Some("this build has no updater".into()),
        }
    }
}

/// Where an install has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Downloading,
    Installing,
    /// Swapped in; the server is restarting into it.
    Restarting,
    /// The last attempt failed, and why. Nothing was changed.
    Failed(String),
}

impl Phase {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Downloading => "downloading",
            Self::Installing => "installing",
            Self::Restarting => "restarting",
            Self::Failed(_) => "failed",
        }
    }
}

/// A release on offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub version: String,
    pub notes_url: Option<String>,
    pub published_at: DateTime<Utc>,
}

/// What the admin page shows.
#[derive(Debug, Clone)]
pub struct UpdateStatus {
    /// False when there is no updater (a container).
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    pub current: String,
    pub edition: Edition,
    pub mode: UpdateMode,
    pub hour: u8,
    pub available: Option<Available>,
    pub checked_at: Option<DateTime<Utc>>,
    /// Why the last check failed.
    pub error: Option<String>,
    /// Whether a release could be installed here, or why not.
    pub installable: Result<(), String>,
    pub phase: Phase,
}

/// A release on offer, as this install would put it in place.
#[derive(Debug, Clone)]
enum Offer {
    Binary(Manifest),
    Image {
        release: ImageRelease,
        /// The signed text, passed on to the sidecar, which checks it again.
        text: String,
        /// Credentials for a private registry, from the licence service.
        registry_auth: Option<serde_json::Value>,
    },
}

impl Offer {
    fn available(&self) -> Available {
        match self {
            Self::Binary(m) => Available {
                version: m.version.clone(),
                notes_url: m.notes_url.clone(),
                published_at: m.published_at,
            },
            Self::Image { release, .. } => Available {
                version: release.version.clone(),
                notes_url: release.notes_url.clone(),
                published_at: release.published_at,
            },
        }
    }
}

#[derive(Default)]
struct State {
    offered: Option<Offer>,
    checked_at: Option<DateTime<Utc>>,
    error: Option<String>,
    phase: Option<Phase>,
}

pub struct Updates {
    db: Db,
    settings: SettingsHandle,
    licensing: Option<Arc<Licensing>>,
    config: UpdatesConfig,
    http: reqwest::Client,
    state: Mutex<State>,
    restart: tokio::sync::watch::Sender<bool>,
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

impl Updates {
    pub fn new(
        db: Db,
        settings: SettingsHandle,
        licensing: Option<Arc<Licensing>>,
        config: UpdatesConfig,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .connect_timeout(std::time::Duration::from_secs(20))
            .user_agent(format!("haloid-server/{}", config.current))
            .build()
            .unwrap_or_default();
        Arc::new(Self {
            db,
            settings,
            licensing,
            config,
            http,
            state: Mutex::new(State::default()),
            restart: tokio::sync::watch::channel(false).0,
        })
    }

    pub fn settings(&self) -> &SettingsHandle {
        &self.settings
    }

    /// Becomes true once a new binary is in place: the server should stop
    /// gracefully and start it.
    pub fn restart_requested(&self) -> tokio::sync::watch::Receiver<bool> {
        self.restart.subscribe()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Administrators only.
    pub fn status(&self, actor: Actor) -> Result<UpdateStatus, DomainError> {
        if !actor.is_admin() {
            return Err(DomainError::Forbidden);
        }
        let settings = self.settings.get();
        let state = self.state();
        Ok(UpdateStatus {
            enabled: self.config.disabled.is_none(),
            disabled_reason: self.config.disabled.clone(),
            current: self.config.current.clone(),
            edition: self.config.edition,
            mode: settings.updates.mode,
            hour: settings.updates.hour,
            available: state.offered.as_ref().map(Offer::available),
            checked_at: state.checked_at,
            error: state.error.clone(),
            installable: self.installable(),
            phase: state.phase.clone().unwrap_or(Phase::Idle),
        })
    }

    /// Could a release be installed here? A binary must be known and its
    /// directory writable, since the new one is written beside it; a
    /// container needs the updater sidecar.
    pub fn installable(&self) -> Result<(), String> {
        if let Some(reason) = &self.config.disabled {
            return Err(reason.clone());
        }
        let exe = match &self.config.installer {
            Installer::Sidecar { .. } => return Ok(()),
            Installer::Container => {
                return Err(
                    "this container has no updater sidecar (HALOID_UPDATER_URL); \
                     pull the new image: docker compose pull && docker compose up -d"
                        .into(),
                );
            }
            Installer::Binary { exe: None } => {
                return Err("the running binary could not be found".into());
            }
            Installer::Binary { exe: Some(exe) } => exe,
        };
        let dir = exe.parent().unwrap_or(Path::new("."));
        let probe = dir.join(format!(".haloid-write-check-{}", std::process::id()));
        match std::fs::File::create(&probe) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                Ok(())
            }
            Err(error) => Err(format!(
                "{} is not writable by the server ({error}); see docs/deploy.md, Updates",
                dir.display()
            )),
        }
    }

    /// Look for a release now. A release that does not apply (not newer,
    /// another target) is not an error: there is simply nothing on offer.
    pub async fn check(&self) -> Result<Option<Available>, DomainError> {
        if let Some(reason) = &self.config.disabled {
            return Err(DomainError::invalid_param("updates", reason.clone()));
        }
        let result = self.latest().await;
        let sidecar = self.sidecar_failure().await;
        let now = Utc::now();
        let mut state = self.state();
        state.checked_at = Some(now);
        if let Some(failure) = sidecar
            && state.phase.is_none()
        {
            state.phase = Some(Phase::Failed(failure));
        }
        match result {
            Ok(found) => {
                let expect = Expect {
                    edition: self.config.edition,
                    target: &self.config.target,
                    current: &self.config.current,
                };
                state.error = None;
                state.offered = found.filter(|offer| match offer {
                    Offer::Binary(m) => m.applies(&expect).is_ok(),
                    Offer::Image { release, .. } => release
                        .applies(self.config.edition, &self.config.current)
                        .is_ok(),
                });
                Ok(state.offered.as_ref().map(Offer::available))
            }
            Err(error) => {
                tracing::info!(%error, "looking for a Haloid release failed");
                state.error = Some(error.clone());
                state.offered = None;
                Err(DomainError::invalid_param("updates", error))
            }
        }
    }

    /// The newest release, checked against the trusted keys: a binary, or
    /// in a container an image.
    async fn latest(&self) -> Result<Option<Offer>, String> {
        let (text, registry_auth) = match &self.config.source {
            Source::Gitlab { api } => (self.ask_gitlab(api).await?, None),
            Source::LicenceService { server } => {
                match self.ask_licence_service(server.as_deref()).await? {
                    Some((text, auth)) => (Some(text), auth),
                    None => (None, None),
                }
            }
        };
        let Some(text) = text else { return Ok(None) };
        if self.config.installer.images() {
            let release = haloid_release::verify_image(&text, &self.config.trusted)
                .map_err(|e| e.to_string())?;
            return Ok(Some(Offer::Image {
                release,
                text,
                registry_auth,
            }));
        }
        haloid_release::verify(&text, &self.config.trusted)
            .map(|m| Some(Offer::Binary(m)))
            .map_err(|e| e.to_string())
    }

    /// What the sidecar says about its last update, if that failed: after
    /// it puts the old container back, this is how the admin page hears.
    async fn sidecar_failure(&self) -> Option<String> {
        let Installer::Sidecar { url, token } = &self.config.installer else {
            return None;
        };
        #[derive(serde::Deserialize)]
        struct Status {
            phase: String,
            version: Option<String>,
            detail: Option<String>,
        }
        let bytes = self
            .http
            .get(format!("{}/v1/status", url.trim_end_matches('/')))
            .bearer_auth(token)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .ok()?
            .bytes()
            .await
            .ok()?;
        let status: Status = serde_json::from_slice(&bytes).ok()?;
        (status.phase == "failed").then(|| {
            format!(
                "updating to {} failed: {}",
                status.version.unwrap_or_default(),
                status.detail.unwrap_or_default()
            )
        })
    }

    async fn ask_gitlab(&self, api: &str) -> Result<Option<String>, String> {
        #[derive(serde::Deserialize)]
        struct Release {
            assets: Assets,
        }
        #[derive(serde::Deserialize)]
        struct Assets {
            links: Vec<Link>,
        }
        #[derive(serde::Deserialize)]
        struct Link {
            name: String,
            direct_asset_url: String,
        }
        let response = self
            .http
            .get(format!(
                "{}/releases/permalink/latest",
                api.trim_end_matches('/')
            ))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let bytes = response
            .error_for_status()
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        let release: Release = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let name = if self.config.installer.images() {
            format!("haloid-image-{}.manifest", self.config.edition.as_str())
        } else {
            format!(
                "haloid-server-{}-{}.manifest",
                self.config.edition.as_str(),
                self.config.target
            )
        };
        let Some(link) = release.assets.links.into_iter().find(|l| l.name == name) else {
            return Ok(None);
        };
        self.http
            .get(link.direct_asset_url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map(Some)
            .map_err(|e| e.to_string())
    }

    /// The release, and for an image the credentials to pull it with.
    async fn ask_licence_service(
        &self,
        server: Option<&str>,
    ) -> Result<Option<(String, Option<serde_json::Value>)>, String> {
        let Some(server) = server else {
            return Err(
                "this instance is not pointed at a licence service (HALOID_LICENCE_SERVER)".into(),
            );
        };
        let Some(licensing) = &self.licensing else {
            return Ok(None);
        };
        let Some(licence) = licensing.licence_id() else {
            return Err("Pro updates need a licence key".into());
        };
        #[derive(serde::Deserialize)]
        struct Answer {
            manifest: Option<String>,
            registry_auth: Option<serde_json::Value>,
        }
        let target = if self.config.installer.images() {
            "oci"
        } else {
            self.config.target.as_str()
        };
        let body = serde_json::json!({
            "licence": licence,
            "instance": licensing.instance_id(),
            "version": self.config.current,
            "target": target,
        });
        let bytes = self
            .http
            .post(format!("{}/api/v1/update", server.trim_end_matches('/')))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        let answer: Answer = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok(answer.manifest.map(|m| (m, answer.registry_auth)))
    }

    /// Start installing the release on offer. Returns once it has started;
    /// [`Updates::status`] follows it.
    pub async fn install(self: &Arc<Self>, by: By) -> Result<String, DomainError> {
        if !by.actor.is_admin() {
            return Err(DomainError::Forbidden);
        }
        self.begin(by.actor.user_id().map(|u| u.0), by.ip).await
    }

    async fn begin(
        self: &Arc<Self>,
        actor: Option<u64>,
        ip: Option<std::net::IpAddr>,
    ) -> Result<String, DomainError> {
        self.installable()
            .map_err(|reason| DomainError::invalid_param("updates", reason))?;
        let offer = {
            let mut state = self.state();
            if matches!(
                state.phase,
                Some(Phase::Downloading | Phase::Installing | Phase::Restarting)
            ) {
                return Err(DomainError::Conflict(
                    "an update is already under way".into(),
                ));
            }
            let Some(offer) = state.offered.clone() else {
                return Err(DomainError::invalid_param(
                    "updates",
                    "no release is on offer",
                ));
            };
            state.phase = Some(Phase::Downloading);
            offer
        };
        let version = offer.available().version;
        audit::record(
            &self.db,
            &audit::NewEntry {
                actor: actor.map(haloid_core::ids::UserId),
                action: "update.install",
                target: None,
                target_id: None,
                detail: serde_json::json!({
                    "from": self.config.current,
                    "to": version,
                }),
                ip,
            },
        )
        .await
        .map_err(database)?;
        match offer {
            Offer::Binary(manifest) => self.install_binary(manifest),
            Offer::Image {
                text,
                registry_auth,
                ..
            } => {
                if let Err(error) = self.hand_to_sidecar(&text, registry_auth).await {
                    tracing::error!(%error, %version, "the updater sidecar did not take the update");
                    self.state().phase = Some(Phase::Failed(error.clone()));
                    return Err(DomainError::invalid_param("updates", error));
                }
                tracing::warn!(from = %self.config.current, to = %version, "the updater sidecar is replacing this container");
                self.state().phase = Some(Phase::Restarting);
            }
        }
        Ok(version)
    }

    fn install_binary(self: &Arc<Self>, manifest: Manifest) {
        let this = self.clone();
        tokio::spawn(async move {
            let Installer::Binary { exe: Some(exe) } = this.config.installer.clone() else {
                unreachable!("installable checked the binary");
            };
            match this.put_in_place(&manifest, &exe).await {
                Ok(()) => {
                    tracing::warn!(from = %this.config.current, to = %manifest.version, "update installed; restarting");
                    this.state().phase = Some(Phase::Restarting);
                    let _ = this.restart.send(true);
                }
                Err(error) => {
                    tracing::error!(%error, version = %manifest.version, "the update failed; nothing was changed");
                    this.state().phase = Some(Phase::Failed(error));
                }
            }
        });
    }

    /// Give the signed release to the sidecar, which checks it itself and
    /// replaces this container. This process is stopped by it.
    async fn hand_to_sidecar(
        &self,
        release: &str,
        registry_auth: Option<serde_json::Value>,
    ) -> Result<(), String> {
        let Installer::Sidecar { url, token } = &self.config.installer else {
            return Err("there is no updater sidecar".into());
        };
        let body = serde_json::json!({ "release": release, "registry_auth": registry_auth });
        let response = self
            .http
            .post(format!("{}/v1/update", url.trim_end_matches('/')))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| format!("the updater sidecar did not answer: {e}"))?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status();
        let said = response.text().await.unwrap_or_default();
        let reason = serde_json::from_str::<serde_json::Value>(&said)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or(said);
        Err(format!("the updater sidecar refused ({status}): {reason}"))
    }

    /// Download, check, and swap in. On any failure the running binary is
    /// untouched and the download removed.
    async fn put_in_place(&self, manifest: &Manifest, exe: &Path) -> Result<(), String> {
        let dir = exe.parent().unwrap_or(Path::new("."));
        let name = exe
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("haloid-server");
        let download = dir.join(format!(".{name}.download-{}", std::process::id()));
        let result = self.fetch_and_check(manifest, &download).await;
        let result = match result {
            Ok(()) => {
                self.state().phase = Some(Phase::Installing);
                swap(exe, &download, &dir.join(format!("{name}.previous")))
            }
            Err(error) => Err(error),
        };
        if result.is_err() {
            let _ = tokio::fs::remove_file(&download).await;
        }
        result
    }

    async fn fetch_and_check(&self, manifest: &Manifest, to: &Path) -> Result<(), String> {
        let response = self
            .http
            .get(&manifest.url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| format!("downloading {}: {e}", manifest.url))?;
        let mut file = tokio::fs::File::create(to)
            .await
            .map_err(|e| format!("writing {}: {e}", to.display()))?;
        let mut hash = Sha256::new();
        let mut size: u64 = 0;
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| format!("downloading: {e}"))?;
            size += chunk.len() as u64;
            if size > manifest.size {
                return Err("the download is larger than the release says".into());
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("writing the download: {e}"))?;
        }
        file.sync_all().await.map_err(|e| e.to_string())?;
        drop(file);
        if size != manifest.size || hex::encode(hash.finalize()) != manifest.sha256 {
            return Err("the download does not match the release's size and checksum".into());
        }
        set_executable(to)?;
        // The new binary must say what it is before it replaces this one.
        let output = tokio::process::Command::new(to)
            .arg("--version")
            .output()
            .await
            .map_err(|e| format!("running the new binary: {e}"))?;
        let said = String::from_utf8_lossy(&output.stdout);
        let expected = format!(
            "haloid-server {} {}",
            manifest.version,
            manifest.edition.as_str()
        );
        if !output.status.success() || !said.trim().starts_with(&expected) {
            return Err(format!(
                "the new binary reports version {:?}, not {expected:?}",
                said.trim()
            ));
        }
        Ok(())
    }

    /// One wake of the background task: look if a look is due, and in
    /// `auto` install at the configured hour.
    pub async fn tick(self: &Arc<Self>, now: DateTime<Utc>) {
        if self.config.disabled.is_some() {
            return;
        }
        let settings = self.settings.get();
        if settings.updates.mode == UpdateMode::Off {
            return;
        }
        let due = self
            .state()
            .checked_at
            .is_none_or(|at| now - at >= CHECK_EVERY);
        if due {
            let _ = self.check().await;
        }
        // A failed attempt waits for a person, rather than retrying hourly.
        let ready = {
            let state = self.state();
            state.offered.is_some() && !matches!(state.phase, Some(Phase::Failed(_)))
        };
        if settings.updates.mode == UpdateMode::Auto
            && now.hour() == u32::from(settings.updates.hour)
            && ready
            && let Err(error) = self.begin(None, None).await
        {
            tracing::info!(%error, "not installing the update automatically");
        }
    }

    /// Wake every [`TICK`] for as long as the process runs.
    pub fn spawn(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.tick(Utc::now()).await;
                tokio::time::sleep(TICK).await;
            }
        })
    }
}

fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("making the new binary executable: {e}"))
}

/// Keep the running binary as `previous`, and rename the new one over it.
/// Renames within one directory, so each step is atomic.
fn swap(exe: &Path, new: &Path, previous: &Path) -> Result<(), String> {
    // A hard link keeps the old binary under its new name while the
    // original path is still there, so there is never a moment without one.
    let _ = std::fs::remove_file(previous);
    std::fs::hard_link(exe, previous)
        .or_else(|_| std::fs::copy(exe, previous).map(|_| ()))
        .map_err(|e| format!("keeping the old binary: {e}"))?;
    std::fs::rename(new, exe).map_err(|e| format!("putting the new binary in place: {e}"))
}

/// Put `<exe>.previous` back: for `haloid-server rollback`.
pub fn roll_back(exe: &Path) -> Result<(), String> {
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("haloid-server");
    let previous = exe.with_file_name(format!("{name}.previous"));
    if !previous.exists() {
        return Err(format!("there is no {} to go back to", previous.display()));
    }
    std::fs::rename(&previous, exe).map_err(|e| format!("restoring {}: {e}", previous.display()))
}
