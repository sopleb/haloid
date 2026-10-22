//! The login sessions repository.
//!
//! A session is found by the sha256 of its cookie token. Generating and
//! hashing the token is the auth layer's job; the token itself never reaches
//! this crate, so a database dump cannot be replayed as a cookie.

use crate::users::{self, UserRow};
use crate::{Db, ip};
use chrono::{DateTime, Utc};
use haloid_core::account::{Session, User};
use haloid_core::ids::{SessionId, UserId};
use std::net::IpAddr;

#[derive(Debug, Clone)]
pub struct NewSession<'a> {
    pub user_id: UserId,
    /// Lowercase hex sha256 of the cookie token.
    pub token_hash: &'a str,
    pub ip: Option<IpAddr>,
    /// Truncated to the column's 255 characters rather than failing the
    /// login over a long header.
    pub user_agent: Option<&'a str>,
    pub expires_at: DateTime<Utc>,
}

const USER_AGENT_MAX: usize = 255;

/// Aliased so they can share a row with [`users::COLUMNS`].
const COLUMNS: &str = "s.id AS session_id, s.user_id, s.ip, s.user_agent, \
     s.created_at AS session_created_at, s.last_seen_at, s.expires_at";

#[derive(sqlx::FromRow)]
struct SessionRow {
    session_id: String,
    user_id: u64,
    ip: Option<Vec<u8>>,
    user_agent: Option<String>,
    session_created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl From<SessionRow> for Session {
    fn from(row: SessionRow) -> Self {
        Session {
            id: SessionId(row.session_id),
            user_id: UserId(row.user_id),
            ip: row.ip.as_deref().and_then(ip::from_bytes),
            user_agent: row.user_agent,
            created_at: row.session_created_at,
            last_seen_at: row.last_seen_at,
            expires_at: row.expires_at,
        }
    }
}

pub async fn create(db: &Db, new: &NewSession<'_>) -> Result<Session, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let user_agent: Option<String> = new
        .user_agent
        .map(|ua| ua.chars().take(USER_AGENT_MAX).collect());

    sqlx::query(
        "INSERT INTO ih_sessions (id, token_hash, user_id, ip, user_agent, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(new.token_hash)
    .bind(new.user_id.0)
    .bind(new.ip.map(ip::to_bytes))
    .bind(user_agent)
    .bind(new.expires_at)
    .execute(db)
    .await?;

    let sql = format!("SELECT {COLUMNS} FROM ih_sessions s WHERE s.id = ?");
    let row: SessionRow = sqlx::query_as(&sql).bind(&id).fetch_one(db).await?;
    Ok(row.into())
}

/// The session for a token hash, with its user, if it has not expired at
/// `now`. One query: this runs on every cookie-authenticated request.
///
/// Whether the user may still act — banned, locked — is the caller's call;
/// the user comes back as is so it can be decided in one place.
pub async fn find_active(
    db: &Db,
    token_hash: &str,
    now: DateTime<Utc>,
) -> Result<Option<(Session, User)>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        session: SessionRow,
        #[sqlx(flatten)]
        user: UserRow,
    }

    let sql = format!(
        "SELECT {COLUMNS}, {users} FROM ih_sessions s
         JOIN ih_users u ON u.id = s.user_id
         WHERE s.token_hash = ? AND s.expires_at > ?",
        users = users::COLUMNS,
    );
    let Some(row) = sqlx::query_as::<_, Row>(&sql)
        .bind(token_hash)
        .bind(now)
        .fetch_optional(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some((row.session.into(), row.user.into_domain()?)))
}

/// A user's sessions, most recently used first, for the account page.
pub async fn list_for_user(db: &Db, user_id: UserId) -> Result<Vec<Session>, sqlx::Error> {
    let sql = format!(
        "SELECT {COLUMNS} FROM ih_sessions s WHERE s.user_id = ?
         ORDER BY s.last_seen_at DESC, s.created_at DESC"
    );
    let rows: Vec<SessionRow> = sqlx::query_as(&sql).bind(user_id.0).fetch_all(db).await?;
    Ok(rows.into_iter().map(Session::from).collect())
}

/// Record activity. Returns whether the session exists.
pub async fn touch(db: &Db, id: &SessionId, at: DateTime<Utc>) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("UPDATE ih_sessions SET last_seen_at = ? WHERE id = ?")
        .bind(at)
        .bind(id.as_str())
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Log one session out. Returns whether it existed.
pub async fn delete(db: &Db, id: &SessionId) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM ih_sessions WHERE id = ?")
        .bind(id.as_str())
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Log a user out everywhere, optionally sparing one session — the one
/// making the request, after a password change.
pub async fn delete_for_user(
    db: &Db,
    user_id: UserId,
    except: Option<&SessionId>,
) -> Result<u64, sqlx::Error> {
    let done = sqlx::query("DELETE FROM ih_sessions WHERE user_id = ? AND id <> COALESCE(?, '')")
        .bind(user_id.0)
        .bind(except.map(SessionId::as_str))
        .execute(db)
        .await?;
    Ok(done.rows_affected())
}

/// The `ExpireSessions` job: remove every session dead at `now`.
pub async fn delete_expired(db: &Db, now: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    let done = sqlx::query("DELETE FROM ih_sessions WHERE expires_at <= ?")
        .bind(now)
        .execute(db)
        .await?;
    Ok(done.rows_affected())
}
