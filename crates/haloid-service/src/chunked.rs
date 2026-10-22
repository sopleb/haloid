//! Chunked uploads: a big file sent in pieces, any piece resent at will,
//! assembled and then put through the very same pipeline as a single
//! upload.
//!
//! A session is opened with the file's size and its upload fields; the size
//! is held to the caller's limit before a byte arrives, and the pipeline
//! holds the assembled file to it again. Each chunk must be exactly the
//! length its number implies, so a session can never assemble into
//! something other than what was declared. The server's record of which
//! chunks arrived is the only one that counts: a client resuming asks.
//!
//! A guest's session is reachable by whoever holds its id — a random UUID,
//! like the delete token. A member's session answers only to that member.

use crate::settings::SettingsHandle;
use crate::upload::{UploadCommand, UploadSource, UploadedFile, Uploads};
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::auth::Actor;
use haloid_db::Db;
use haloid_db::upload_sessions::{self, MAX_CHUNKS, NewSession, SessionStatus, UploadSession};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// Every chunk but the last is this long.
pub const CHUNK_SIZE: u32 = 8 * 1024 * 1024;
/// How long a session may sit unfinished.
pub const SESSION_LIFETIME: Duration = Duration::from_secs(24 * 3600);

/// The upload fields, held until `complete`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionFields {
    pub title: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub album: Option<String>,
    pub nsfw: Option<bool>,
    pub expires_in_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct OpenSession {
    pub filename: String,
    pub size: u64,
    pub fields: SessionFields,
}

/// A session as its uploader sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionState {
    pub id: String,
    pub chunk_size: u32,
    pub chunks_total: u32,
    pub received: Vec<u32>,
    pub missing: Vec<u32>,
    pub status: SessionStatus,
    pub expires_at: DateTime<Utc>,
}

impl From<&UploadSession> for SessionState {
    fn from(s: &UploadSession) -> Self {
        Self {
            id: s.id.clone(),
            chunk_size: s.chunk_size,
            chunks_total: s.chunks_total,
            received: s.received(),
            missing: s.missing(),
            status: s.status,
            expires_at: s.expires_at,
        }
    }
}

pub struct ChunkedUploads {
    db: Db,
    settings: SettingsHandle,
    uploads: Arc<Uploads>,
    dir: PathBuf,
    chunk_size: u32,
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

fn io(error: std::io::Error) -> DomainError {
    DomainError::Internal(Box::new(error))
}

/// Whether a failed `complete` is our fault and worth trying again, as
/// opposed to the file being refused. An unreadable or unsupported file is
/// refused however often it is sent.
fn is_retryable(error: &DomainError) -> bool {
    use haloid_core::MediaError;
    match error {
        DomainError::Database(_)
        | DomainError::Internal(_)
        | DomainError::Storage(_)
        | DomainError::StorageFull => true,
        DomainError::Media(MediaError::ProbeFailed(_) | MediaError::Unsupported(_)) => false,
        DomainError::Media(_) => true,
        _ => false,
    }
}

fn chunk_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{n:05}"))
}

impl ChunkedUploads {
    pub fn new(db: Db, settings: SettingsHandle, uploads: Arc<Uploads>) -> Self {
        let dir = uploads.temp_dir().join("sessions");
        Self {
            db,
            settings,
            uploads,
            dir,
            chunk_size: CHUNK_SIZE,
        }
    }

    /// Smaller chunks, so tests can exercise many with small files.
    pub fn with_chunk_size(mut self, chunk_size: u32) -> Self {
        self.chunk_size = chunk_size.max(1);
        self
    }

    /// A session the actor may use: theirs, or a guest one for a guest.
    async fn session(
        &self,
        actor: Actor,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<UploadSession, DomainError> {
        let session = upload_sessions::find(&self.db, id)
            .await
            .map_err(database)?
            .filter(|s| s.user_id == actor.user_id())
            .filter(|s| s.expires_at > now && s.status != SessionStatus::Expired)
            .ok_or(DomainError::NotFound("upload session"))?;
        Ok(session)
    }

    pub async fn open(
        &self,
        actor: Actor,
        request: OpenSession,
        ip: Option<IpAddr>,
        now: DateTime<Utc>,
    ) -> Result<SessionState, DomainError> {
        let settings = self.settings.get();
        if !settings.may_upload(actor) {
            return Err(DomainError::Forbidden);
        }
        // Sessions hold chunks on disk until they complete or expire, so
        // opening them is held to the upload rate too.
        self.uploads.check_rate("session", actor, ip)?;
        let limit = settings.max_upload_size(actor);
        if request.size == 0 {
            return Err(DomainError::EmptySource);
        }
        if request.size > limit {
            return Err(DomainError::SizeLimit {
                size: request.size,
                limit,
            });
        }
        if upload_sessions::chunks_for(request.size, self.chunk_size) > MAX_CHUNKS {
            return Err(DomainError::invalid_param(
                "size",
                "too large for one session",
            ));
        }
        let filename = request.filename.trim();
        if filename.is_empty() || filename.chars().count() > 255 {
            return Err(DomainError::invalid_param(
                "filename",
                "is required, at most 255 characters",
            ));
        }
        // A bad album should fail now, not after the whole file is sent.
        self.uploads
            .resolve_album(actor, request.fields.album.as_deref())
            .await?;

        let id = uuid::Uuid::new_v4().to_string();
        let dir = self.dir.join(&id);
        tokio::fs::create_dir_all(&dir).await.map_err(io)?;
        let expires_at = now + chrono::Duration::from_std(SESSION_LIFETIME).unwrap_or_default();
        let session = upload_sessions::create(
            &self.db,
            &NewSession {
                id: &id,
                user_id: actor.user_id(),
                filename,
                declared_size: request.size,
                chunk_size: self.chunk_size,
                temp_path: &dir.display().to_string(),
                params: serde_json::to_value(&request.fields)
                    .map_err(|e| DomainError::Internal(e.into()))?,
                expires_at,
            },
        )
        .await
        .map_err(database)?;
        Ok(SessionState::from(&session))
    }

    /// Take chunk `n`. It must be exactly as long as its place implies;
    /// sending a chunk again replaces it.
    pub async fn put_chunk(
        &self,
        actor: Actor,
        id: &str,
        n: u32,
        mut body: impl AsyncRead + Unpin,
        now: DateTime<Utc>,
    ) -> Result<SessionState, DomainError> {
        let session = self.session(actor, id, now).await?;
        if session.status != SessionStatus::Open {
            return Err(DomainError::Conflict(
                "the upload is already complete".into(),
            ));
        }
        if n >= session.chunks_total {
            return Err(DomainError::invalid_param(
                "chunk",
                format!("this upload has chunks 0 to {}", session.chunks_total - 1),
            ));
        }
        let expected = session.chunk_len(n);
        let dir = PathBuf::from(&session.temp_path);
        let partial = dir.join(format!("{n:05}.part-{}", uuid::Uuid::new_v4().simple()));
        let written = async {
            let mut out = tokio::fs::File::create(&partial).await.map_err(io)?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut total: u64 = 0;
            loop {
                let read = body.read(&mut buf).await.map_err(io)?;
                if read == 0 {
                    break;
                }
                total += read as u64;
                if total > expected {
                    break;
                }
                out.write_all(&buf[..read]).await.map_err(io)?;
            }
            out.flush().await.map_err(io)?;
            Ok::<u64, DomainError>(total)
        }
        .await;
        let written = match written {
            Ok(total) if total == expected => total,
            Ok(total) => {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(DomainError::invalid_param(
                    "chunk",
                    format!(
                        "chunk {n} must be {expected} bytes{}",
                        if total > expected {
                            ", and this was longer".to_string()
                        } else {
                            format!(", not {total}")
                        }
                    ),
                ));
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(error);
            }
        };
        tokio::fs::rename(&partial, chunk_path(&dir, n))
            .await
            .map_err(io)?;
        let session = upload_sessions::mark_chunk(&self.db, id, n, written)
            .await
            .map_err(database)?
            .ok_or(DomainError::Conflict(
                "the upload is already complete".into(),
            ))?;
        Ok(SessionState::from(&session))
    }

    pub async fn state(
        &self,
        actor: Actor,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<SessionState, DomainError> {
        Ok(SessionState::from(&self.session(actor, id, now).await?))
    }

    /// Assemble and upload. Only one `complete` runs for a session; a
    /// second while it runs is a conflict. A refusal by the pipeline (the
    /// wrong type, a stop word) ends the session; a fault of ours leaves it
    /// open, so the same call can be tried again.
    pub async fn complete(
        &self,
        actor: Actor,
        id: &str,
        uploader_ip: Option<IpAddr>,
        now: DateTime<Utc>,
    ) -> Result<UploadedFile, DomainError> {
        let session = self.session(actor, id, now).await?;
        match session.status {
            SessionStatus::Open => {}
            SessionStatus::Assembling => {
                return Err(DomainError::Conflict(
                    "the upload is already being completed".into(),
                ));
            }
            _ => {
                return Err(DomainError::Conflict(
                    "the upload is already complete".into(),
                ));
            }
        }
        let missing = session.missing();
        if !missing.is_empty() {
            return Err(DomainError::invalid_param(
                "chunks",
                format!(
                    "{} chunks have not arrived, the first is {}",
                    missing.len(),
                    missing[0]
                ),
            ));
        }
        if !upload_sessions::transition(
            &self.db,
            id,
            SessionStatus::Open,
            SessionStatus::Assembling,
        )
        .await
        .map_err(database)?
        {
            return Err(DomainError::Conflict(
                "the upload is already being completed".into(),
            ));
        }

        let result = self.assemble_and_upload(actor, &session, uploader_ip).await;
        match &result {
            Ok(uploaded) => {
                upload_sessions::finish(&self.db, id, uploaded.file.id)
                    .await
                    .map_err(database)?;
                let _ = tokio::fs::remove_dir_all(&session.temp_path).await;
            }
            Err(error) if !is_retryable(error) => {
                upload_sessions::transition(
                    &self.db,
                    id,
                    SessionStatus::Assembling,
                    SessionStatus::Failed,
                )
                .await
                .map_err(database)?;
                let _ = tokio::fs::remove_dir_all(&session.temp_path).await;
            }
            Err(_) => {
                upload_sessions::transition(
                    &self.db,
                    id,
                    SessionStatus::Assembling,
                    SessionStatus::Open,
                )
                .await
                .map_err(database)?;
            }
        }
        result
    }

    async fn assemble_and_upload(
        &self,
        actor: Actor,
        session: &UploadSession,
        uploader_ip: Option<IpAddr>,
    ) -> Result<UploadedFile, DomainError> {
        let dir = PathBuf::from(&session.temp_path);
        let whole = dir.join("assembled");
        {
            let mut out = tokio::fs::File::create(&whole).await.map_err(io)?;
            for n in 0..session.chunks_total {
                let mut chunk = tokio::fs::File::open(chunk_path(&dir, n))
                    .await
                    .map_err(io)?;
                tokio::io::copy(&mut chunk, &mut out).await.map_err(io)?;
            }
            out.flush().await.map_err(io)?;
        }
        let fields: SessionFields =
            serde_json::from_value(session.params.clone()).unwrap_or_default();
        let reader = tokio::fs::File::open(&whole).await.map_err(io)?;
        self.uploads
            .upload(UploadCommand {
                actor,
                source: UploadSource::Stream {
                    reader: Box::new(reader),
                    filename: Some(session.filename.clone()),
                },
                title: fields.title,
                description: fields.description,
                tags: fields.tags,
                album: fields.album,
                category: None,
                nsfw: fields.nsfw,
                expiration: fields
                    .expires_in_secs
                    .filter(|s| *s > 0)
                    .map(Duration::from_secs),
                width: None,
                uploader_ip,
            })
            .await
    }

    /// Give up on a session and its chunks.
    pub async fn abandon(
        &self,
        actor: Actor,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        let session = self.session(actor, id, now).await?;
        if session.status == SessionStatus::Assembling {
            return Err(DomainError::Conflict(
                "the upload is being completed".into(),
            ));
        }
        let _ = tokio::fs::remove_dir_all(&session.temp_path).await;
        upload_sessions::delete(&self.db, id)
            .await
            .map_err(database)?;
        Ok(())
    }

    /// The `ExpireUploadSessions` job: drop the chunks of sessions left
    /// unfinished past their lifetime. Returns how many.
    pub async fn expire(&self, now: DateTime<Utc>) -> Result<usize, DomainError> {
        let mut count = 0;
        loop {
            let batch = upload_sessions::expired(&self.db, now, 100)
                .await
                .map_err(database)?;
            if batch.is_empty() {
                return Ok(count);
            }
            for session in batch {
                let _ = tokio::fs::remove_dir_all(&session.temp_path).await;
                upload_sessions::mark_expired(&self.db, &session.id)
                    .await
                    .map_err(database)?;
                count += 1;
            }
        }
    }
}
