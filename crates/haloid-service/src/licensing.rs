//! The licence this instance runs under, and what it allows.
//!
//! The instance gets its id at first start. An administrator installs a key
//! from the admin area, or the operator sets `HALOID_LICENCE_KEY`, which
//! wins and is never written to the database. Keys are checked offline,
//! against the public keys compiled into the server; an online check is
//! designed in (`docs/plan.md`, chunk 24) but waits for the licence service.
//!
//! [`Licensing::entitlements`] is what the rest of the service consults.
//! It is worked out at the moment of asking, so a key lapses on its date
//! without anything having to run.

use crate::admin::By;
use arc_swap::ArcSwapOption;
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::auth::Actor;
use haloid_core::entitlements::{Entitlements, LicenceKind};
use haloid_db::{Db, audit, instance};
use haloid_licence::{Licence, LicenceError};
use std::sync::Arc;

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

/// The licence as the admin area shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenceStatus {
    pub instance_id: String,
    pub entitlements: Entitlements,
    /// The installed licence, when there is one for this instance.
    pub licence: Option<LicenceSummary>,
    /// Whether it came from `HALOID_LICENCE_KEY`, and so cannot be changed
    /// from the admin area.
    pub from_environment: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenceSummary {
    pub id: String,
    pub licensee: String,
    pub kind: LicenceKind,
    pub issued_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl From<&Licence> for LicenceSummary {
    fn from(licence: &Licence) -> Self {
        Self {
            id: licence.id.clone(),
            licensee: licence.licensee.clone(),
            kind: licence.kind.into(),
            issued_at: licence.issued_at,
            expires_at: licence.expires_at,
        }
    }
}

pub struct Licensing {
    db: Db,
    instance_id: String,
    trusted: Vec<[u8; 32]>,
    from_environment: bool,
    current: ArcSwapOption<Licence>,
    /// The licence service's last word on the current key: revoked or not.
    /// Only ever set by an answer; silence changes nothing.
    revoked: std::sync::atomic::AtomicBool,
}

/// What the licence service says about a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnlineStatus {
    Active,
    Revoked,
    /// It has no record of the key, as for one issued offline: no change.
    Unknown,
}

/// A fresh instance id: `inst_` and twenty base62 characters.
fn new_instance_id() -> String {
    format!(
        "inst_{}{}",
        haloid_id::generate_public_id(),
        haloid_id::generate_public_id()
    )
}

impl Licensing {
    /// Read (or, at first start, make) the instance id, and the licence:
    /// `env_key` if given, else the key installed from the admin area. A
    /// key that does not check out is logged and ignored, never fatal: the
    /// instance runs as Community rather than not at all.
    pub async fn load(
        db: Db,
        trusted: Vec<[u8; 32]>,
        env_key: Option<String>,
    ) -> Result<Self, DomainError> {
        let row = instance::ensure(&db, &new_instance_id())
            .await
            .map_err(database)?;
        let from_environment = env_key.is_some();
        let key = env_key.or(row.licence_key);
        let current = match key {
            Some(key) => match haloid_licence::verify_for(&key, &row.instance_id, &trusted) {
                Ok(licence) => {
                    tracing::info!(
                        licence = %licence.id,
                        licensee = %licence.licensee,
                        kind = LicenceKind::from(licence.kind).as_str(),
                        "licence loaded"
                    );
                    Some(Arc::new(licence))
                }
                Err(error) => {
                    tracing::warn!(%error, "the licence key is not usable; running as Community");
                    None
                }
            },
            None => None,
        };
        Ok(Self {
            db,
            instance_id: row.instance_id,
            trusted,
            from_environment,
            current: ArcSwapOption::new(current),
            revoked: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The installed key's id, if there is one.
    pub fn licence_id(&self) -> Option<String> {
        self.current.load().as_ref().map(|l| l.id.clone())
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// What this instance may do at `now`.
    pub fn entitlements(&self, now: DateTime<Utc>) -> Entitlements {
        match self.current.load().as_deref() {
            None => Entitlements::COMMUNITY,
            Some(_) if self.revoked.load(std::sync::atomic::Ordering::Relaxed) => {
                Entitlements::REVOKED
            }
            Some(licence) => licence.entitlements(now),
        }
    }

    /// Ask the licence service about the current key. Only a definite
    /// answer changes anything: an unreachable service, an error or an
    /// unknown key leaves the offline result in force, so the service being
    /// down never costs a customer their licence.
    pub async fn check_online(
        &self,
        http: &reqwest::Client,
        server: &str,
    ) -> Result<Option<OnlineStatus>, String> {
        let Some(licence) = self.current.load_full() else {
            return Ok(None);
        };
        #[derive(serde::Deserialize)]
        struct Answer {
            status: OnlineStatus,
        }
        let body = serde_json::json!({
            "licence": licence.id,
            "instance": self.instance_id,
            "version": env!("CARGO_PKG_VERSION"),
        });
        let bytes = http
            .post(format!("{}/api/v1/check", server.trim_end_matches('/')))
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
        match answer.status {
            OnlineStatus::Revoked => {
                if !self
                    .revoked
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    tracing::warn!(licence = %licence.id, "the licence service has revoked this key");
                }
            }
            OnlineStatus::Active => self
                .revoked
                .store(false, std::sync::atomic::Ordering::Relaxed),
            OnlineStatus::Unknown => {}
        }
        Ok(Some(answer.status))
    }

    /// Check now and then every `period`, for as long as the process runs.
    pub fn spawn_online_checks(
        self: &Arc<Self>,
        server: String,
        period: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        let licensing = self.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .unwrap_or_default();
            loop {
                if let Err(error) = licensing.check_online(&http, &server).await {
                    tracing::info!(%error, "the licence service could not be reached; the offline check stands");
                }
                tokio::time::sleep(period).await;
            }
        })
    }

    /// Administrators only.
    pub fn status(&self, actor: Actor, now: DateTime<Utc>) -> Result<LicenceStatus, DomainError> {
        if !actor.is_admin() {
            return Err(DomainError::Forbidden);
        }
        Ok(LicenceStatus {
            instance_id: self.instance_id.clone(),
            entitlements: self.entitlements(now),
            licence: self.current.load().as_deref().map(LicenceSummary::from),
            from_environment: self.from_environment,
        })
    }

    /// Install a key from the admin area. It must check out and be for this
    /// instance; it takes effect at once.
    pub async fn install(
        &self,
        by: By,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<LicenceStatus, DomainError> {
        self.writable(by.actor)?;
        let licence =
            haloid_licence::verify_for(key, &self.instance_id, &self.trusted).map_err(|error| {
                let reason = match error {
                    LicenceError::Malformed => "is not a Haloid licence key".to_string(),
                    LicenceError::BadSignature => "is not signed by Haloid".to_string(),
                    LicenceError::OtherInstance(other) => format!(
                        "is for another instance ({other}); this one is {}",
                        self.instance_id
                    ),
                };
                DomainError::invalid_param("key", reason)
            })?;
        instance::set_licence_key(&self.db, Some(key.trim()))
            .await
            .map_err(database)?;
        self.record(
            by,
            "licence.install",
            serde_json::json!({
                "licence": licence.id,
                "licensee": licence.licensee,
                "kind": LicenceKind::from(licence.kind).as_str(),
            }),
        )
        .await?;
        self.current.store(Some(Arc::new(licence)));
        // A new key starts with a clean slate until the service says otherwise.
        self.revoked
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.status(by.actor, now)
    }

    /// Remove the installed key: the instance runs as Community.
    pub async fn remove(&self, by: By) -> Result<(), DomainError> {
        self.writable(by.actor)?;
        instance::set_licence_key(&self.db, None)
            .await
            .map_err(database)?;
        let removed = self.current.swap(None);
        self.record(
            by,
            "licence.remove",
            serde_json::json!({ "licence": removed.map(|l| l.id.clone()) }),
        )
        .await
    }

    fn writable(&self, actor: Actor) -> Result<(), DomainError> {
        if !actor.is_admin() {
            return Err(DomainError::Forbidden);
        }
        if self.from_environment {
            return Err(DomainError::invalid_param(
                "key",
                "is set by HALOID_LICENCE_KEY; change it there",
            ));
        }
        Ok(())
    }

    async fn record(
        &self,
        by: By,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), DomainError> {
        audit::record(
            &self.db,
            &audit::NewEntry {
                actor: by.actor.user_id(),
                action,
                target: None,
                target_id: None,
                detail,
                ip: by.ip,
            },
        )
        .await
        .map_err(database)
    }
}
