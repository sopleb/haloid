//! Authentication: who is making a request.
//!
//! Three ways in. A browser holds a session cookie; an API client sends an
//! `hld_` key; anyone else is a guest. Login and registration are behind
//! three independent defences, all of which must pass: per-IP rate limits,
//! ALTCHA proof-of-work, and — for login — per-account lockout after
//! repeated failures.
//!
//! Every failure that could reveal whether an account exists says the same
//! thing (`InvalidCredentials`) and takes the same time: a login naming no
//! account still pays for an argon2 verification against a decoy hash.

pub mod account;
pub mod human;
pub mod limits;
pub mod secrets;

use crate::settings::SettingsHandle;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use haloid_core::DomainError;
use haloid_core::account::{
    ApiKey, Session, User, UserStatus, validate_email, validate_password, validate_username,
};
use haloid_core::auth::{Actor, Role};
use haloid_core::ids::{ApiKeyId, SessionId, UserId};
use haloid_db::{Db, api_keys, sessions, users};
use human::Altcha;
use limits::RateLimiter;
use std::net::IpAddr;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub altcha: human::AltchaConfig,
    pub session_lifetime: Duration,
    /// Failed logins that lock an account.
    pub lockout_threshold: u32,
    pub lockout_duration: Duration,
}

impl AuthConfig {
    pub fn new(altcha_secret: String) -> Self {
        Self {
            altcha: human::AltchaConfig::with_secret(altcha_secret),
            session_lifetime: Duration::from_secs(30 * 86_400),
            lockout_threshold: 5,
            lockout_duration: Duration::from_secs(15 * 60),
        }
    }
}

pub struct LoginRequest<'a> {
    /// A username, or an email address.
    pub login: &'a str,
    pub password: &'a str,
    /// The ALTCHA widget's form field.
    pub altcha: &'a str,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<&'a str>,
}

pub struct RegisterRequest<'a> {
    pub username: &'a str,
    pub email: Option<&'a str>,
    pub password: &'a str,
    pub altcha: &'a str,
    pub ip: Option<IpAddr>,
}

/// A successful login. `token` goes in the cookie and is never stored.
#[derive(Debug)]
pub struct LoggedIn {
    pub user: User,
    pub session: Session,
    pub token: String,
}

pub struct Auth {
    db: Db,
    settings: SettingsHandle,
    altcha: Altcha,
    config: AuthConfig,
    login_limit: RateLimiter,
    register_limit: RateLimiter,
    challenge_limit: RateLimiter,
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, DomainError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| DomainError::Internal(e.to_string().into()))
}

fn ip_key(scope: &str, ip: Option<IpAddr>) -> String {
    ip.map_or_else(|| format!("{scope}:unknown"), |ip| format!("{scope}:{ip}"))
}

fn chrono(duration: Duration) -> ChronoDuration {
    ChronoDuration::from_std(duration).unwrap_or(ChronoDuration::MAX)
}

impl Auth {
    pub fn new(db: Db, settings: SettingsHandle, config: AuthConfig) -> Self {
        Self {
            altcha: Altcha::new(config.altcha.clone()),
            // Ten attempts, then one a minute, per address.
            login_limit: RateLimiter::new(10, Duration::from_secs(60)),
            register_limit: RateLimiter::new(3, Duration::from_secs(600)),
            // Challenges are cheap to ask for and a key derivation to make.
            challenge_limit: RateLimiter::new(30, Duration::from_secs(10)),
            db,
            settings,
            config,
        }
    }

    /// A signed proof-of-work challenge for the login and registration forms.
    pub fn challenge(
        &self,
        ip: Option<IpAddr>,
        now: DateTime<Utc>,
    ) -> Result<serde_json::Value, DomainError> {
        self.challenge_limit
            .check(&ip_key("challenge", ip), Instant::now())?;
        self.altcha.challenge(now.timestamp())
    }

    pub async fn register(
        &self,
        request: &RegisterRequest<'_>,
        now: DateTime<Utc>,
    ) -> Result<User, DomainError> {
        if !self.settings.get().site.registration_open {
            return Err(DomainError::RegistrationClosed);
        }
        let per_hour = self.settings.get().site.registrations_per_hour;
        self.register_limit.check_rate(
            &ip_key("register", request.ip),
            Instant::now(),
            per_hour,
            Duration::from_secs(3600 / u64::from(per_hour.max(1))),
        )?;
        validate_username(request.username)?;
        if let Some(email) = request.email {
            validate_email(email)?;
        }
        validate_password(request.password)?;
        self.altcha.verify(request.altcha, now.timestamp())?;

        let password = request.password.to_string();
        let hash = blocking(move || secrets::hash_password(&password)).await??;
        let user = users::create(
            &self.db,
            &users::NewUser {
                username: request.username,
                email: request.email,
                password_hash: Some(&hash),
                role: Role::User,
                status: UserStatus::Valid,
            },
        )
        .await?;
        // The first account on an instance administers it, as with most
        // self-hosted software: there is no installer to make one. Decided
        // after the insert, by id, so two registrations racing on an empty
        // instance cannot both win.
        if users::is_first(&self.db, user.id).await.map_err(database)? {
            users::set_role(&self.db, user.id, Role::Admin)
                .await
                .map_err(database)?;
            tracing::info!(username = %user.username, "first account registered: made administrator");
            return users::find_by_id(&self.db, user.id)
                .await
                .map_err(database)?
                .ok_or(DomainError::Unauthenticated);
        }
        Ok(user)
    }

    pub async fn login(
        &self,
        request: &LoginRequest<'_>,
        now: DateTime<Utc>,
    ) -> Result<LoggedIn, DomainError> {
        self.login_limit
            .check(&ip_key("login", request.ip), Instant::now())?;
        self.altcha.verify(request.altcha, now.timestamp())?;

        let record = users::find_for_login(&self.db, request.login)
            .await
            .map_err(database)?;
        let password = request.password.to_string();
        let Some(record) = record else {
            blocking(move || secrets::verify_against_decoy(&password)).await?;
            return Err(DomainError::InvalidCredentials);
        };

        // Locked accounts are refused before the password is even looked
        // at, so guessing during a lockout gains nothing.
        if let Some(until) = record.user.locked_until.filter(|&until| until > now) {
            return Err(DomainError::AccountLocked {
                retry_after_secs: (until - now).num_seconds().max(1) as u64,
            });
        }

        let stored = record.password_hash.clone();
        let typed = password.clone();
        let matches = match record.password_hash {
            Some(hash) => blocking(move || secrets::verify_password(&password, &hash)).await?,
            None => {
                blocking(move || secrets::verify_against_decoy(&password)).await?;
                false
            }
        };
        if !matches {
            users::record_failed_login(
                &self.db,
                record.user.id,
                self.config.lockout_threshold,
                now + chrono(self.config.lockout_duration),
            )
            .await
            .map_err(database)?;
            return Err(DomainError::InvalidCredentials);
        }

        // The right password for a banned account still lets nobody in.
        if record.user.status == UserStatus::Banned {
            return Err(DomainError::Forbidden);
        }

        // An imported account's bcrypt hash becomes argon2id now that the
        // password is known.
        if stored.as_deref().is_some_and(secrets::needs_rehash) {
            let hash = blocking(move || secrets::hash_password(&typed)).await??;
            users::set_password_hash(&self.db, record.user.id, Some(&hash))
                .await
                .map_err(database)?;
        }
        users::record_login(&self.db, record.user.id, now)
            .await
            .map_err(database)?;
        let token = secrets::new_session_token();
        let session = sessions::create(
            &self.db,
            &sessions::NewSession {
                user_id: record.user.id,
                token_hash: &secrets::sha256_hex(&token),
                ip: request.ip,
                user_agent: request.user_agent,
                expires_at: now + chrono(self.config.session_lifetime),
            },
        )
        .await
        .map_err(database)?;
        let user = users::find_by_id(&self.db, record.user.id)
            .await
            .map_err(database)?
            .ok_or(DomainError::Unauthenticated)?;
        Ok(LoggedIn {
            user,
            session,
            token,
        })
    }

    /// The actor behind a session cookie. `None` for an unknown or expired
    /// token, or a banned user — a cookie never outlives a ban.
    pub async fn session_actor(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<(Actor, Session)>, DomainError> {
        let found = sessions::find_active(&self.db, &secrets::sha256_hex(token), now)
            .await
            .map_err(database)?;
        let Some((session, user)) = found else {
            return Ok(None);
        };
        if user.status == UserStatus::Banned {
            return Ok(None);
        }
        // Record activity at most every five minutes, not on every request.
        if now - session.last_seen_at > ChronoDuration::minutes(5) {
            sessions::touch(&self.db, &session.id, now)
                .await
                .map_err(database)?;
        }
        Ok(Some((user.actor(), session)))
    }

    /// The user behind an actor, for "who am I". `None` for a guest.
    pub async fn user(&self, actor: Actor) -> Result<Option<User>, DomainError> {
        match actor.user_id() {
            Some(id) => users::find_by_id(&self.db, id).await.map_err(database),
            None => Ok(None),
        }
    }

    /// How long a new session lasts, for the cookie's `Max-Age`.
    pub fn session_lifetime(&self) -> Duration {
        self.config.session_lifetime
    }

    pub async fn logout(&self, session: &SessionId) -> Result<(), DomainError> {
        sessions::delete(&self.db, session)
            .await
            .map_err(database)?;
        Ok(())
    }

    /// Change a password, given the current one, and log out every other
    /// session: whoever might have known the old password is out.
    pub async fn change_password(
        &self,
        user: UserId,
        current_session: &SessionId,
        current: &str,
        new: &str,
    ) -> Result<(), DomainError> {
        validate_password(new)?;
        let record = users::find_by_id(&self.db, user)
            .await
            .map_err(database)?
            .ok_or(DomainError::Unauthenticated)?;
        let stored = users::find_for_login(&self.db, &record.username)
            .await
            .map_err(database)?
            .and_then(|r| r.password_hash);

        let current = current.to_string();
        let matches = match stored {
            Some(hash) => blocking(move || secrets::verify_password(&current, &hash)).await?,
            None => false,
        };
        if !matches {
            return Err(DomainError::InvalidCredentials);
        }

        let new = new.to_string();
        let hash = blocking(move || secrets::hash_password(&new)).await??;
        users::set_password_hash(&self.db, user, Some(&hash))
            .await
            .map_err(database)?;
        sessions::delete_for_user(&self.db, user, Some(current_session))
            .await
            .map_err(database)?;
        Ok(())
    }

    /// Mint an API key for `owner`, or an instance-wide guest key with
    /// `None`. The returned string is the only time the full key exists.
    pub async fn create_api_key(
        &self,
        owner: Option<UserId>,
        label: &str,
    ) -> Result<(ApiKey, String), DomainError> {
        let minted = secrets::mint_api_key();
        let key = api_keys::create(
            &self.db,
            &api_keys::NewApiKey {
                user_id: owner,
                label,
                key_prefix: &minted.prefix,
                secret_hash: &minted.secret_hash,
                hmac_key: &minted.hmac_key,
            },
        )
        .await?;
        Ok((key, minted.key))
    }

    pub async fn revoke_api_key(
        &self,
        id: ApiKeyId,
        now: DateTime<Utc>,
    ) -> Result<bool, DomainError> {
        api_keys::revoke(&self.db, id, now).await.map_err(database)
    }

    /// The actor behind an API key. Distinguishes a missing key, an unknown
    /// or wrong one, and a revoked one, because the legacy API reports them
    /// with different codes.
    pub async fn api_key_actor(&self, key: &str, now: DateTime<Utc>) -> Result<Actor, DomainError> {
        if key.trim().is_empty() {
            return Err(DomainError::MissingApiKey);
        }
        let (prefix, secret) = secrets::split_api_key(key).ok_or(DomainError::InvalidApiKey)?;
        let record = api_keys::find_by_prefix(&self.db, prefix)
            .await
            .map_err(database)?
            .ok_or(DomainError::InvalidApiKey)?;
        if !secrets::hashes_match(
            &secrets::key_hash(&record.hmac_key, secret),
            &record.secret_hash,
        ) {
            return Err(DomainError::InvalidApiKey);
        }
        // Only a holder of the real secret learns that the key is revoked.
        if !record.key.is_active {
            return Err(DomainError::RevokedApiKey);
        }

        let actor = match record.key.user_id {
            None => Actor::Guest,
            Some(owner) => {
                let user = users::find_by_id(&self.db, owner)
                    .await
                    .map_err(database)?
                    .ok_or(DomainError::InvalidApiKey)?;
                if user.status == UserStatus::Banned {
                    return Err(DomainError::Forbidden);
                }
                user.actor()
            }
        };
        api_keys::touch(&self.db, record.key.id, now)
            .await
            .map_err(database)?;
        Ok(actor)
    }
}
