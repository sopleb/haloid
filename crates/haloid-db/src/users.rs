//! The users repository.
//!
//! Free functions over [`Db`], like the settings repository. Usernames and
//! emails keep the spelling the user typed; uniqueness and lookup go through
//! the `_lower` columns, which this module is the only writer of.
//!
//! Validation (reserved names, allowed characters, length) is the service
//! layer's. This layer stores what it is given.

use crate::error::{DbError, unique_violation};
use crate::{Db, decode_enum};
use chrono::{DateTime, Utc};
use haloid_core::account::{User, UserStatus};
use haloid_core::auth::Role;
use haloid_core::ids::UserId;

/// What a new account needs. Everything else starts at its column default.
#[derive(Debug, Clone)]
pub struct NewUser<'a> {
    pub username: &'a str,
    pub email: Option<&'a str>,
    /// An argon2id PHC string. `None` is an account with no password login.
    pub password_hash: Option<&'a str>,
    pub role: Role,
    pub status: UserStatus,
}

/// A user together with their password hash, for the login path only. Kept
/// out of [`User`] so the hash cannot ride along anywhere else.
#[derive(Debug, Clone)]
pub struct LoginRecord {
    pub user: User,
    pub password_hash: Option<String>,
}

/// Columns for [`UserRow`], qualified so they can be joined against.
pub(crate) const COLUMNS: &str = "u.id, u.id_encoded, u.username, u.email, u.role, u.status, \
     u.display_name, u.is_private, u.file_count, u.album_count, u.disk_used, \
     u.failed_logins, u.locked_until, u.created_at, u.last_login_at";

#[derive(sqlx::FromRow)]
pub(crate) struct UserRow {
    id: u64,
    id_encoded: String,
    username: String,
    email: Option<String>,
    role: String,
    status: String,
    display_name: Option<String>,
    is_private: bool,
    file_count: u32,
    album_count: u32,
    disk_used: u64,
    failed_logins: u32,
    locked_until: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    last_login_at: Option<DateTime<Utc>>,
}

impl UserRow {
    pub(crate) fn into_domain(self) -> Result<User, sqlx::Error> {
        Ok(User {
            id: UserId(self.id),
            id_encoded: self.id_encoded.into(),
            username: self.username,
            email: self.email,
            role: decode_enum("role", &self.role)?,
            status: decode_enum("status", &self.status)?,
            display_name: self.display_name,
            is_private: self.is_private,
            file_count: self.file_count,
            album_count: self.album_count,
            disk_used: self.disk_used,
            failed_logins: self.failed_logins,
            locked_until: self.locked_until,
            created_at: self.created_at,
            last_login_at: self.last_login_at,
        })
    }
}

/// How many times to draw a fresh public id after a collision. At ~59 bits a
/// second collision is not going to happen; the loop exists so the first one
/// is not a user-visible error.
const ID_ATTEMPTS: usize = 3;

/// Create an account. A taken username or email is
/// [`DbError::Conflict`] naming the field.
pub async fn create(db: &Db, new: &NewUser<'_>) -> Result<User, DbError> {
    for _ in 0..ID_ATTEMPTS {
        let result = sqlx::query(
            "INSERT INTO ih_users
               (id_encoded, username, username_lower, email, email_lower,
                password_hash, role, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(haloid_id::generate_public_id())
        .bind(new.username)
        .bind(new.username.to_lowercase())
        .bind(new.email)
        .bind(new.email.map(str::to_lowercase))
        .bind(new.password_hash)
        .bind(new.role.as_str())
        .bind(new.status.as_str())
        .execute(db)
        .await;

        match result {
            Ok(done) => {
                let id = UserId(done.last_insert_id());
                return find_by_id(db, id)
                    .await?
                    .ok_or(DbError::Sqlx(sqlx::Error::RowNotFound));
            }
            Err(e) => match unique_violation(&e).as_deref() {
                Some("uq_users_id_encoded") => continue,
                Some("uq_users_username") => return Err(DbError::Conflict("username")),
                Some("uq_users_email") => return Err(DbError::Conflict("email")),
                _ => return Err(e.into()),
            },
        }
    }
    Err(DbError::Conflict("id_encoded"))
}

/// Users for the admin view, by id, after `after`, optionally only those
/// whose username or email contains `search`.
pub async fn list(
    db: &Db,
    search: Option<&str>,
    after: Option<UserId>,
    limit: u32,
) -> Result<Vec<User>, sqlx::Error> {
    let pattern = search.filter(|s| !s.is_empty()).map(|s| {
        format!(
            "%{}%",
            s.to_lowercase().replace('%', "\\%").replace('_', "\\_")
        )
    });
    let sql = format!(
        "SELECT {COLUMNS} FROM ih_users u
         WHERE u.id > ? AND (? IS NULL OR u.username_lower LIKE ? OR u.email_lower LIKE ?)
         ORDER BY u.id LIMIT ?"
    );
    sqlx::query_as::<_, UserRow>(&sql)
        .bind(after.map_or(0, |u| u.0))
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .bind(limit)
        .fetch_all(db)
        .await?
        .into_iter()
        .map(UserRow::into_domain)
        .collect()
}

async fn find_where(db: &Db, condition: &str, value: &str) -> Result<Option<User>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_users u WHERE {condition}");
    sqlx::query_as::<_, UserRow>(&sql)
        .bind(value)
        .fetch_optional(db)
        .await?
        .map(UserRow::into_domain)
        .transpose()
}

pub async fn find_by_id(db: &Db, id: UserId) -> Result<Option<User>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_users u WHERE u.id = ?");
    sqlx::query_as::<_, UserRow>(&sql)
        .bind(id.0)
        .fetch_optional(db)
        .await?
        .map(UserRow::into_domain)
        .transpose()
}

pub async fn find_by_id_encoded(db: &Db, id_encoded: &str) -> Result<Option<User>, sqlx::Error> {
    find_where(db, "u.id_encoded = ?", id_encoded).await
}

/// Case-insensitive.
pub async fn find_by_username(db: &Db, username: &str) -> Result<Option<User>, sqlx::Error> {
    find_where(db, "u.username_lower = ?", &username.to_lowercase()).await
}

/// The login lookup: a username, or an email if `login` contains `@`.
///
/// One column or the other, never both. Searching both would let a user whose
/// username equals someone else's email match two rows, and which one won
/// would be up to the query planner.
pub async fn find_for_login(db: &Db, login: &str) -> Result<Option<LoginRecord>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        user: UserRow,
        password_hash: Option<String>,
    }

    let column = if login.contains('@') {
        "u.email_lower"
    } else {
        "u.username_lower"
    };
    let sql = format!("SELECT {COLUMNS}, u.password_hash FROM ih_users u WHERE {column} = ?");
    let Some(row) = sqlx::query_as::<_, Row>(&sql)
        .bind(login.to_lowercase())
        .fetch_optional(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(LoginRecord {
        user: row.user.into_domain()?,
        password_hash: row.password_hash,
    }))
}

/// Whether the user exists. Every setter below returns this.
async fn update(db: &Db, id: UserId, sql: &str, value: Option<&str>) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(sql).bind(value).bind(id.0).execute(db).await?;
    Ok(done.rows_affected() > 0)
}

/// Whether `id` is the oldest account, and no administrator exists — the
/// first registration on a fresh instance.
pub async fn is_first(db: &Db, id: UserId) -> Result<bool, sqlx::Error> {
    let (older, admins): (i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT COUNT(*) FROM ih_users WHERE id < ?),
           (SELECT COUNT(*) FROM ih_users WHERE role = 'admin')",
    )
    .bind(id.0)
    .fetch_one(db)
    .await?;
    Ok(older == 0 && admins == 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile<'a> {
    pub display_name: Option<&'a str>,
    pub email: Option<&'a str>,
    pub is_private: bool,
}

/// Replace what a user may change about themselves. An email someone else
/// holds is `Conflict("email")`. Returns whether the user exists.
pub async fn update_profile(db: &Db, id: UserId, profile: &Profile<'_>) -> Result<bool, DbError> {
    let result = sqlx::query(
        "UPDATE ih_users SET display_name = ?, email = ?, email_lower = ?, is_private = ?
         WHERE id = ?",
    )
    .bind(profile.display_name)
    .bind(profile.email)
    .bind(profile.email.map(str::to_lowercase))
    .bind(profile.is_private)
    .bind(id.0)
    .execute(db)
    .await;
    match result {
        Ok(done) => Ok(done.rows_affected() > 0),
        Err(e) => match unique_violation(&e).as_deref() {
            Some("uq_users_email") => Err(DbError::Conflict("email")),
            _ => Err(e.into()),
        },
    }
}

/// `None` removes password login from the account.
///
/// Returns whether the user exists. MariaDB reports matched rather than
/// changed rows to sqlx, so setting the same value twice is still `true`.
pub async fn set_password_hash(
    db: &Db,
    id: UserId,
    hash: Option<&str>,
) -> Result<bool, sqlx::Error> {
    update(
        db,
        id,
        "UPDATE ih_users SET password_hash = ? WHERE id = ?",
        hash,
    )
    .await
}

pub async fn set_role(db: &Db, id: UserId, role: Role) -> Result<bool, sqlx::Error> {
    update(
        db,
        id,
        "UPDATE ih_users SET role = ? WHERE id = ?",
        Some(role.as_str()),
    )
    .await
}

pub async fn set_status(db: &Db, id: UserId, status: UserStatus) -> Result<bool, sqlx::Error> {
    update(
        db,
        id,
        "UPDATE ih_users SET status = ? WHERE id = ?",
        Some(status.as_str()),
    )
    .await
}

/// Count a failed login, and lock the account until `lock_until` if this
/// attempt reaches `threshold`.
///
/// One statement, so two concurrent failures cannot both read the old count.
/// `locked_until` is assigned first and from `failed_logins + 1`: MariaDB
/// applies assignments left to right by default but simultaneously under
/// `SIMULTANEOUS_ASSIGNMENT`, and this order is correct under both.
pub async fn record_failed_login(
    db: &Db,
    id: UserId,
    threshold: u32,
    lock_until: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ih_users
         SET locked_until = IF(failed_logins + 1 >= ?, ?, locked_until),
             failed_logins = failed_logins + 1
         WHERE id = ?",
    )
    .bind(threshold)
    .bind(lock_until)
    .bind(id.0)
    .execute(db)
    .await?;
    Ok(())
}

/// Lift a lockout without counting a login, for the operator's rescue
/// command.
pub async fn clear_lockout(db: &Db, id: UserId) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE ih_users SET failed_logins = 0, locked_until = NULL WHERE id = ?")
        .bind(id.0)
        .execute(db)
        .await?;
    Ok(())
}

/// A successful login: clears the failure count and any lockout.
pub async fn record_login(db: &Db, id: UserId, at: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ih_users
         SET failed_logins = 0, locked_until = NULL, last_login_at = ?
         WHERE id = ?",
    )
    .bind(at)
    .bind(id.0)
    .execute(db)
    .await?;
    Ok(())
}

/// Add to (or, with negative deltas, subtract from) a user's file count and
/// disk usage.
///
/// `SET x = x + ?` in one statement, never read-modify-write, so concurrent
/// uploads cannot lose an increment. The columns are unsigned, so the
/// arithmetic is done signed and floored at zero: a decrement that overshoots
/// — a retried purge — must not fail the statement.
pub async fn adjust_usage(db: &Db, id: UserId, files: i64, bytes: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ih_users
         SET file_count = GREATEST(CAST(file_count AS SIGNED) + ?, 0),
             disk_used  = GREATEST(CAST(disk_used AS SIGNED) + ?, 0)
         WHERE id = ?",
    )
    .bind(files)
    .bind(bytes)
    .bind(id.0)
    .execute(db)
    .await?;
    Ok(())
}

/// As [`adjust_usage`], for the album count.
pub async fn adjust_album_count(db: &Db, id: UserId, delta: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ih_users
         SET album_count = GREATEST(CAST(album_count AS SIGNED) + ?, 0)
         WHERE id = ?",
    )
    .bind(delta)
    .bind(id.0)
    .execute(db)
    .await?;
    Ok(())
}
