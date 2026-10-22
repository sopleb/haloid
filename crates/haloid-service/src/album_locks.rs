//! Password albums: proving the password once, and the signed grant that
//! remembers it.
//!
//! A grant is `{expiry}.{mac}`, the MAC an HMAC-SHA256 over the album's id,
//! its current password hash and the expiry, keyed from the instance
//! secret. Changing the album's password changes the hash and so voids
//! every grant made under the old one; nothing is stored server-side.
//! Guessing is rate limited per address and album, like a login.

use crate::auth::limits::RateLimiter;
use crate::auth::secrets::verify_password;
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::files::{Album, AlbumPrivacy};
use haloid_core::ids::AlbumId;
use haloid_db::{Db, albums};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

/// How long an unlock lasts.
pub const GRANT_LIFETIME: Duration = Duration::from_secs(24 * 3600);

pub struct AlbumLocks {
    db: Db,
    key: [u8; 32],
    limit: RateLimiter,
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

/// The cookie that carries an album's grant.
pub fn cookie_name(album: &str) -> String {
    format!("haloid_album_{album}")
}

impl AlbumLocks {
    pub fn new(db: Db, secret: &str) -> Self {
        let key: [u8; 32] = Sha256::digest(format!("album-unlock:{secret}")).into();
        Self {
            db,
            key,
            limit: RateLimiter::new(10, Duration::from_secs(60)),
        }
    }

    fn mac(&self, album: &Album, hash: &str, expiry: i64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("any key length works");
        mac.update(format!("{}\n{hash}\n{expiry}", album.id_encoded).as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    async fn password_album(&self, id_encoded: &str) -> Result<(Album, String), DomainError> {
        let album = albums::find_by_id_encoded(&self.db, id_encoded)
            .await
            .map_err(database)?
            .filter(|a| a.privacy == AlbumPrivacy::Password)
            .ok_or(DomainError::NotFound("album"))?;
        let hash = albums::password_hash(&self.db, album.id)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("album"))?;
        Ok((album, hash))
    }

    /// Check the password; on success, a grant to set as the album's cookie.
    pub async fn unlock(
        &self,
        id_encoded: &str,
        password: &str,
        ip: Option<IpAddr>,
        now: DateTime<Utc>,
    ) -> Result<(String, Duration), DomainError> {
        let who = ip.map_or_else(|| "unknown".to_string(), |ip| ip.to_string());
        self.limit
            .check(&format!("{who}:{id_encoded}"), Instant::now())?;
        let (album, hash) = self.password_album(id_encoded).await?;
        let (password, stored) = (password.to_string(), hash.clone());
        let ok = tokio::task::spawn_blocking(move || verify_password(&password, &stored))
            .await
            .map_err(|e| DomainError::Internal(e.to_string().into()))?;
        if !ok {
            return Err(DomainError::InvalidCredentials);
        }
        let expiry = now.timestamp() + GRANT_LIFETIME.as_secs() as i64;
        Ok((
            format!("{expiry}.{}", self.mac(&album, &hash, expiry)),
            GRANT_LIFETIME,
        ))
    }

    /// The albums these `(album id, grant)` pairs still unlock.
    pub async fn unlocked(&self, grants: &[(String, String)], now: DateTime<Utc>) -> Vec<AlbumId> {
        let mut open = Vec::new();
        for (id_encoded, grant) in grants.iter().take(32) {
            let Some((expiry, mac)) = grant.split_once('.') else {
                continue;
            };
            let Ok(expiry) = expiry.parse::<i64>() else {
                continue;
            };
            if expiry <= now.timestamp() {
                continue;
            }
            let Ok((album, hash)) = self.password_album(id_encoded).await else {
                continue;
            };
            if bool::from(
                self.mac(&album, &hash, expiry)
                    .as_bytes()
                    .ct_eq(mac.as_bytes()),
            ) {
                open.push(album.id);
            }
        }
        open
    }
}
