//! Getting back in: what the server's operator can do from a shell when no
//! administrator can sign in — the only one forgot their password, or was
//! demoted by mistake.
//!
//! Whoever can run the binary against the database already holds every
//! key, so these act with no web actor. Each one is written to the audit
//! log, with no actor and `"via": "command line"`, so the change is not a
//! silent one.

use crate::auth::secrets;
use haloid_core::DomainError;
use haloid_core::account::{User, UserStatus, validate_password};
use haloid_core::auth::Role;
use haloid_db::{Db, audit, sessions, users};

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

/// A user by username, or by email.
async fn find(db: &Db, who: &str) -> Result<User, DomainError> {
    users::find_for_login(db, who)
        .await
        .map_err(database)?
        .map(|login| login.user)
        .ok_or(DomainError::NotFound("user"))
}

async fn record(db: &Db, action: &str, user: &User) -> Result<(), DomainError> {
    audit::record(
        db,
        &audit::NewEntry {
            actor: None,
            action,
            target: Some("user"),
            target_id: Some(user.id.0),
            detail: serde_json::json!({ "username": user.username, "via": "command line" }),
            ip: None,
        },
    )
    .await
    .map_err(database)
}

/// Make someone an administrator, and lift any ban or lockout so they can
/// sign in.
pub async fn promote(db: &Db, who: &str) -> Result<User, DomainError> {
    let user = find(db, who).await?;
    users::set_role(db, user.id, Role::Admin)
        .await
        .map_err(database)?;
    users::set_status(db, user.id, UserStatus::Valid)
        .await
        .map_err(database)?;
    users::clear_lockout(db, user.id).await.map_err(database)?;
    record(db, "user.promote", &user).await?;
    find(db, who).await
}

/// Set a password, lift any lockout, and sign the user out everywhere.
pub async fn set_password(db: &Db, who: &str, password: &str) -> Result<User, DomainError> {
    validate_password(password)?;
    let user = find(db, who).await?;
    let hash = secrets::hash_password(password)?;
    users::set_password_hash(db, user.id, Some(&hash))
        .await
        .map_err(database)?;
    users::clear_lockout(db, user.id).await.map_err(database)?;
    sessions::delete_for_user(db, user.id, None)
        .await
        .map_err(database)?;
    record(db, "user.password_reset", &user).await?;
    Ok(user)
}

/// Every administrator, to see who can still get in.
pub async fn admins(db: &Db) -> Result<Vec<User>, DomainError> {
    let mut found = Vec::new();
    let mut after = None;
    loop {
        let page = users::list(db, None, after, 200).await.map_err(database)?;
        let Some(last) = page.last() else { break };
        after = Some(last.id);
        found.extend(page.into_iter().filter(|u| u.role == Role::Admin));
    }
    Ok(found)
}
