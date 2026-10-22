//! The API keys repository.
//!
//! A key is `hld_{id}_{secret}`. The prefix up to and including the second
//! underscore is the public lookup handle; only an HMAC of the secret is
//! stored, under a per-key HMAC key. Minting and verifying keys is the auth
//! layer's job — this layer stores and finds what it is given.

use crate::Db;
use crate::error::{DbError, unique_violation};
use chrono::{DateTime, Utc};
use haloid_core::account::ApiKey;
use haloid_core::ids::{ApiKeyId, UserId};

#[derive(Debug, Clone)]
pub struct NewApiKey<'a> {
    /// `None` makes an instance-wide guest key.
    pub user_id: Option<UserId>,
    pub label: &'a str,
    pub key_prefix: &'a str,
    /// Lowercase hex HMAC-SHA256 of the secret.
    pub secret_hash: &'a str,
    pub hmac_key: &'a [u8],
}

/// A key with the material needed to verify a presented secret. Only the
/// authentication path should ever hold one.
#[derive(Debug, Clone)]
pub struct ApiKeyRecord {
    pub key: ApiKey,
    pub secret_hash: String,
    pub hmac_key: Vec<u8>,
}

const COLUMNS: &str = "k.id, k.user_id, k.label, k.key_prefix, k.is_active, \
     k.last_used_at, k.created_at, k.revoked_at";

#[derive(sqlx::FromRow)]
struct KeyRow {
    id: u64,
    user_id: Option<u64>,
    label: String,
    key_prefix: String,
    is_active: bool,
    last_used_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl From<KeyRow> for ApiKey {
    fn from(row: KeyRow) -> Self {
        ApiKey {
            id: ApiKeyId(row.id),
            user_id: row.user_id.map(UserId),
            label: row.label,
            key_prefix: row.key_prefix,
            is_active: row.is_active,
            last_used_at: row.last_used_at,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
        }
    }
}

/// Store a new key. A prefix already in use is [`DbError::Conflict`].
pub async fn create(db: &Db, new: &NewApiKey<'_>) -> Result<ApiKey, DbError> {
    let result = sqlx::query(
        "INSERT INTO ih_api_keys (user_id, label, key_prefix, secret_hash, hmac_key)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(new.user_id.map(|id| id.0))
    .bind(new.label)
    .bind(new.key_prefix)
    .bind(new.secret_hash)
    .bind(new.hmac_key)
    .execute(db)
    .await;

    let id = match result {
        Ok(done) => done.last_insert_id(),
        Err(e) if unique_violation(&e).as_deref() == Some("uq_api_keys_prefix") => {
            return Err(DbError::Conflict("key_prefix"));
        }
        Err(e) => return Err(e.into()),
    };

    let sql = format!("SELECT {COLUMNS} FROM ih_api_keys k WHERE k.id = ?");
    let row: KeyRow = sqlx::query_as(&sql).bind(id).fetch_one(db).await?;
    Ok(row.into())
}

/// The key with this prefix, revoked or not.
///
/// Revoked keys are returned deliberately: "revoked" and "no such key" are
/// different errors to a client, and only the caller can tell them apart if
/// the row comes back.
pub async fn find_by_prefix(db: &Db, prefix: &str) -> Result<Option<ApiKeyRecord>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        key: KeyRow,
        secret_hash: String,
        hmac_key: Vec<u8>,
    }

    let sql = format!(
        "SELECT {COLUMNS}, k.secret_hash, k.hmac_key FROM ih_api_keys k WHERE k.key_prefix = ?"
    );
    Ok(sqlx::query_as::<_, Row>(&sql)
        .bind(prefix)
        .fetch_optional(db)
        .await?
        .map(|row| ApiKeyRecord {
            key: row.key.into(),
            secret_hash: row.secret_hash,
            hmac_key: row.hmac_key,
        }))
}

/// A user's keys, oldest first; `None` lists the guest keys.
pub async fn list(db: &Db, user_id: Option<UserId>) -> Result<Vec<ApiKey>, sqlx::Error> {
    // `<=>` is null-safe equality, so `None` matches exactly the ownerless
    // rows instead of nothing at all.
    let sql = format!(
        "SELECT {COLUMNS} FROM ih_api_keys k WHERE k.user_id <=> ? ORDER BY k.created_at, k.id"
    );
    let rows: Vec<KeyRow> = sqlx::query_as(&sql)
        .bind(user_id.map(|id| id.0))
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(ApiKey::from).collect())
}

/// Revoke a key. Returns whether this call revoked it: `false` for a missing
/// or already revoked key, so the original `revoked_at` is never overwritten.
pub async fn revoke(db: &Db, id: ApiKeyId, at: DateTime<Utc>) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE ih_api_keys SET is_active = 0, revoked_at = ? WHERE id = ? AND is_active = 1",
    )
    .bind(at)
    .bind(id.0)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Record a use of the key.
pub async fn touch(db: &Db, id: ApiKeyId, at: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE ih_api_keys SET last_used_at = ? WHERE id = ?")
        .bind(at)
        .bind(id.0)
        .execute(db)
        .await?;
    Ok(())
}
