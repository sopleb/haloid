//! What a signed-in user manages about themselves: their profile, their API
//! keys and their sessions. Every method acts on the actor's own account
//! only; another user's key or session is `NotFound`.

use super::{Auth, database};
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::account::{ApiKey, Session, User, validate_email};
use haloid_core::auth::Actor;
use haloid_core::ids::{ApiKeyId, SessionId, UserId};
use haloid_db::{api_keys, sessions, users};

/// The most live API keys one account may hold.
pub const MAX_API_KEYS: usize = 50;
const MAX_DISPLAY_NAME: usize = 190;
const MAX_LABEL: usize = 190;

/// A partial edit: `None` keeps a field, `Some(None)` clears it.
#[derive(Debug, Clone, Default)]
pub struct ProfileEdit {
    pub display_name: Option<Option<String>>,
    pub email: Option<Option<String>>,
    pub is_private: Option<bool>,
}

fn signed_in(actor: Actor) -> Result<UserId, DomainError> {
    actor.user_id().ok_or(DomainError::Unauthenticated)
}

fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl Auth {
    pub async fn update_profile(
        &self,
        actor: Actor,
        edit: ProfileEdit,
    ) -> Result<User, DomainError> {
        let id = signed_in(actor)?;
        let user = users::find_by_id(&self.db, id)
            .await
            .map_err(database)?
            .ok_or(DomainError::Unauthenticated)?;
        let display_name = match edit.display_name {
            Some(name) => trimmed(name),
            None => user.display_name.clone(),
        };
        if display_name
            .as_ref()
            .is_some_and(|n| n.chars().count() > MAX_DISPLAY_NAME)
        {
            return Err(DomainError::invalid_param(
                "display_name",
                format!("is longer than {MAX_DISPLAY_NAME} characters"),
            ));
        }
        let email = match edit.email {
            Some(email) => trimmed(email),
            None => user.email.clone(),
        };
        if let Some(email) = &email {
            validate_email(email)?;
        }
        users::update_profile(
            &self.db,
            id,
            &users::Profile {
                display_name: display_name.as_deref(),
                email: email.as_deref(),
                is_private: edit.is_private.unwrap_or(user.is_private),
            },
        )
        .await?;
        users::find_by_id(&self.db, id)
            .await
            .map_err(database)?
            .ok_or(DomainError::Unauthenticated)
    }

    /// The actor's keys, revoked ones included, newest first.
    pub async fn own_api_keys(&self, actor: Actor) -> Result<Vec<ApiKey>, DomainError> {
        let id = signed_in(actor)?;
        api_keys::list(&self.db, Some(id)).await.map_err(database)
    }

    /// A new key for the actor. The full key is returned this once.
    pub async fn create_own_api_key(
        &self,
        actor: Actor,
        label: &str,
    ) -> Result<(ApiKey, String), DomainError> {
        let id = signed_in(actor)?;
        let label = label.trim();
        if label.chars().count() > MAX_LABEL {
            return Err(DomainError::invalid_param(
                "label",
                format!("is longer than {MAX_LABEL} characters"),
            ));
        }
        let live = self
            .own_api_keys(actor)
            .await?
            .iter()
            .filter(|k| k.is_active)
            .count();
        if live >= MAX_API_KEYS {
            return Err(DomainError::invalid_param(
                "label",
                format!("an account may hold {MAX_API_KEYS} keys; revoke one first"),
            ));
        }
        self.create_api_key(Some(id), label).await
    }

    pub async fn revoke_own_api_key(
        &self,
        actor: Actor,
        key: ApiKeyId,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        let mine = self.own_api_keys(actor).await?.iter().any(|k| k.id == key);
        if !mine {
            return Err(DomainError::NotFound("api key"));
        }
        self.revoke_api_key(key, now).await?;
        Ok(())
    }

    /// The actor's sessions, most recently active first.
    pub async fn own_sessions(&self, actor: Actor) -> Result<Vec<Session>, DomainError> {
        let id = signed_in(actor)?;
        sessions::list_for_user(&self.db, id)
            .await
            .map_err(database)
    }

    /// Sign one of the actor's sessions out.
    pub async fn revoke_own_session(&self, actor: Actor, session: &str) -> Result<(), DomainError> {
        let mine = self
            .own_sessions(actor)
            .await?
            .iter()
            .any(|s| s.id.as_str() == session);
        if !mine {
            return Err(DomainError::NotFound("session"));
        }
        sessions::delete(&self.db, &SessionId(session.to_string()))
            .await
            .map_err(database)?;
        Ok(())
    }
}
