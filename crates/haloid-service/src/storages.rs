//! Administering storages. For now: testing one, end to end, from outside.
//!
//! The test writes a probe object, reads it back over the storage's API
//! and over its public URL, quarantines it and checks the quarantine is not
//! publicly reachable, releases it, and deletes it. Each step is reported on
//! its own, because "it does not work" is no help to an operator and "the
//! quarantine prefix is publicly readable" is.

use haloid_core::DomainError;
use haloid_core::auth::Actor;
use haloid_core::ids::StorageId;
use haloid_core::storage::{PutBody, Storage};
use haloid_storage::pool::StoragePool;
use rand::Rng;
use std::sync::Arc;
use std::time::Duration;

const PROBE: &[u8] = b"haloid storage test";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageTest {
    pub ok: bool,
    pub checks: Vec<Check>,
}

pub struct StorageAdmin {
    pool: Arc<StoragePool>,
    http: reqwest::Client,
}

impl StorageAdmin {
    pub fn new(pool: Arc<StoragePool>) -> Self {
        // Admin-configured URLs only, but still: short timeouts, and no
        // redirects, which could carry the request somewhere else.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self { pool, http }
    }

    /// Fetch a URL; `Ok(status, body)` or why not.
    async fn fetch(&self, url: &str) -> Result<(u16, Vec<u8>), String> {
        let response = self.http.get(url).send().await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|e| e.to_string())?;
        Ok((status, body.to_vec()))
    }

    /// Run the probe against one storage. Administrators only.
    pub async fn test(&self, actor: Actor, id: StorageId) -> Result<StorageTest, DomainError> {
        if !actor.is_admin() {
            return Err(if actor.is_guest() {
                DomainError::Unauthenticated
            } else {
                DomainError::Forbidden
            });
        }
        let storage = self.pool.get(id).ok_or(DomainError::NotFound("storage"))?;
        let key = format!(
            "haloid-test/{}.txt",
            hex::encode(rand::rng().random::<[u8; 8]>())
        );
        let mut checks = Vec::new();
        let ok = self.probe(storage.as_ref(), &key, &mut checks).await;
        Ok(StorageTest { ok, checks })
    }

    async fn probe(&self, storage: &dyn Storage, key: &str, checks: &mut Vec<Check>) -> bool {
        let mut record = |name: &'static str, result: Result<String, String>| {
            let ok = result.is_ok();
            checks.push(Check {
                name,
                ok,
                detail: result.unwrap_or_else(|e| e),
            });
            ok
        };

        let wrote = storage
            .put(key, PutBody::Bytes(PROBE.to_vec()), "text/plain")
            .await
            .map(|_| format!("wrote {key}"))
            .map_err(|e| e.to_string());
        if !record("write", wrote) {
            return false;
        }

        let read = match storage.head(key).await {
            Ok(meta) if meta.size == PROBE.len() as u64 => Ok("read it back".to_string()),
            Ok(meta) => Err(format!(
                "read back {} bytes, not {}",
                meta.size,
                PROBE.len()
            )),
            Err(e) => Err(e.to_string()),
        };
        let mut ok = record("read", read);

        let url = storage.url_for(key);
        let public = match self.fetch(&url).await {
            Ok((200, body)) if body == PROBE => Ok(format!("served at {url}")),
            Ok((200, _)) => Err(format!("{url} answered with different bytes")),
            Ok((status, _)) => Err(format!(
                "{url} answered {status}: check the URL prefix and that the files are public"
            )),
            Err(e) => Err(format!("{url} could not be fetched: {e}")),
        };
        ok &= record("public_url", public);

        let quarantined = storage.quarantine(key).await.map_err(|e| e.to_string());
        let quarantined_ok = record(
            "quarantine",
            quarantined.map(|()| "moved into quarantine".into()),
        );
        ok &= quarantined_ok;

        if quarantined_ok {
            let private = match storage.quarantined_public_url(key) {
                None => Ok("quarantine is not reachable by URL".to_string()),
                Some(leak) => match self.fetch(&leak).await {
                    Ok((status, _)) if (200..300).contains(&status) => Err(format!(
                        "{leak} serves quarantined files: restrict the bucket policy to the \
                         served prefix, or use a separate quarantine bucket"
                    )),
                    Ok((status, _)) => Ok(format!("{leak} refused it ({status})")),
                    Err(_) => Ok(format!("{leak} is not reachable")),
                },
            };
            ok &= record("quarantine_private", private);

            let released = storage
                .release(key)
                .await
                .map(|()| "moved back".to_string())
                .map_err(|e| e.to_string());
            ok &= record("release", released);
        }

        let deleted = storage
            .delete(key)
            .await
            .map(|()| "probe removed".to_string())
            .map_err(|e| e.to_string());
        // Whatever happened above, leave nothing behind.
        let _ = storage.delete_quarantined(key).await;
        ok &= record("delete", deleted);
        ok
    }
}
