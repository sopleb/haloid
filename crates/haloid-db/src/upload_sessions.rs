//! Chunked upload sessions in `ih_upload_sessions`.
//!
//! The bitmap records which chunks have arrived, one bit per chunk. It is
//! the server's word on what is received: a client resuming after a failure
//! asks for it rather than trusting its own memory. Setting a bit and
//! counting the bytes happen under the row lock, so two chunks arriving at
//! once cannot lose each other's bit.

use crate::Db;
use chrono::{DateTime, Utc};
use haloid_core::ids::{FileId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Open,
    Assembling,
    Done,
    Failed,
    Expired,
}

impl SessionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Assembling => "assembling",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }

    fn parse(s: &str) -> Result<Self, sqlx::Error> {
        Ok(match s {
            "open" => Self::Open,
            "assembling" => Self::Assembling,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "expired" => Self::Expired,
            other => return Err(crate::decode_error("status", other)),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UploadSession {
    pub id: String,
    pub user_id: Option<UserId>,
    pub filename: String,
    pub declared_size: u64,
    pub received_size: u64,
    pub chunk_size: u32,
    pub chunks_total: u32,
    pub bitmap: Vec<u8>,
    pub temp_path: String,
    pub params: serde_json::Value,
    pub status: SessionStatus,
    pub file_id: Option<FileId>,
    pub expires_at: DateTime<Utc>,
}

impl UploadSession {
    pub fn has_chunk(&self, n: u32) -> bool {
        self.bitmap
            .get((n / 8) as usize)
            .is_some_and(|byte| byte & (1 << (n % 8)) != 0)
    }

    /// Chunk numbers received, in order.
    pub fn received(&self) -> Vec<u32> {
        (0..self.chunks_total)
            .filter(|n| self.has_chunk(*n))
            .collect()
    }

    /// Chunk numbers still to come, in order.
    pub fn missing(&self) -> Vec<u32> {
        (0..self.chunks_total)
            .filter(|n| !self.has_chunk(*n))
            .collect()
    }

    /// The exact length chunk `n` must have: `chunk_size`, except the last.
    pub fn chunk_len(&self, n: u32) -> u64 {
        let start = u64::from(n) * u64::from(self.chunk_size);
        self.declared_size
            .saturating_sub(start)
            .min(u64::from(self.chunk_size))
    }
}

/// How many chunks of `chunk_size` make `size` bytes.
pub fn chunks_for(size: u64, chunk_size: u32) -> u64 {
    size.div_ceil(u64::from(chunk_size.max(1)))
}

/// The most chunks the bitmap column holds.
pub const MAX_CHUNKS: u64 = 4096 * 8;

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    user_id: Option<u64>,
    filename: String,
    declared_size: u64,
    received_size: u64,
    chunk_size: u32,
    chunks_total: u32,
    chunks_bitmap: Vec<u8>,
    temp_path: String,
    params: Option<sqlx::types::Json<serde_json::Value>>,
    status: String,
    file_id: Option<u64>,
    expires_at: DateTime<Utc>,
}

impl Row {
    fn into_session(self) -> Result<UploadSession, sqlx::Error> {
        Ok(UploadSession {
            id: self.id,
            user_id: self.user_id.map(UserId),
            filename: self.filename,
            declared_size: self.declared_size,
            received_size: self.received_size,
            chunk_size: self.chunk_size,
            chunks_total: self.chunks_total,
            bitmap: self.chunks_bitmap,
            temp_path: self.temp_path,
            params: self.params.map(|p| p.0).unwrap_or(serde_json::Value::Null),
            status: SessionStatus::parse(&self.status)?,
            file_id: self.file_id.map(FileId),
            expires_at: self.expires_at,
        })
    }
}

const COLUMNS: &str = "id, user_id, filename, declared_size, received_size, chunk_size, \
     chunks_total, chunks_bitmap, temp_path, params, status, file_id, expires_at";

#[derive(Debug, Clone)]
pub struct NewSession<'a> {
    pub id: &'a str,
    pub user_id: Option<UserId>,
    pub filename: &'a str,
    pub declared_size: u64,
    pub chunk_size: u32,
    pub temp_path: &'a str,
    pub params: serde_json::Value,
    pub expires_at: DateTime<Utc>,
}

pub async fn create(db: &Db, new: &NewSession<'_>) -> Result<UploadSession, sqlx::Error> {
    let total = chunks_for(new.declared_size, new.chunk_size);
    let total =
        u32::try_from(total).map_err(|_| sqlx::Error::Protocol("too many chunks".into()))?;
    let bitmap = vec![0u8; total.div_ceil(8) as usize];
    sqlx::query(
        "INSERT INTO ih_upload_sessions
           (id, user_id, filename, declared_size, chunk_size, chunks_total, chunks_bitmap,
            temp_path, params, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new.id)
    .bind(new.user_id.map(|u| u.0))
    .bind(new.filename)
    .bind(new.declared_size)
    .bind(new.chunk_size)
    .bind(total)
    .bind(&bitmap)
    .bind(new.temp_path)
    .bind(sqlx::types::Json(&new.params))
    .bind(new.expires_at)
    .execute(db)
    .await?;
    find(db, new.id).await?.ok_or(sqlx::Error::RowNotFound)
}

pub async fn find(db: &Db, id: &str) -> Result<Option<UploadSession>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_upload_sessions WHERE id = ?");
    sqlx::query_as::<_, Row>(&sql)
        .bind(id)
        .fetch_optional(db)
        .await?
        .map(Row::into_session)
        .transpose()
}

/// Record chunk `n` as received, `len` bytes long. Receiving the same chunk
/// again changes nothing. Returns the session as it now is, or `None` if it
/// is gone or no longer open.
pub async fn mark_chunk(
    db: &Db,
    id: &str,
    n: u32,
    len: u64,
) -> Result<Option<UploadSession>, sqlx::Error> {
    let mut tx = db.begin().await?;
    let row: Option<(Vec<u8>, String)> = sqlx::query_as(
        "SELECT chunks_bitmap, status FROM ih_upload_sessions WHERE id = ? FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((mut bitmap, status)) = row else {
        return Ok(None);
    };
    if status != "open" {
        return Ok(None);
    }
    let (byte, bit) = ((n / 8) as usize, 1u8 << (n % 8));
    if let Some(slot) = bitmap.get_mut(byte)
        && *slot & bit == 0
    {
        *slot |= bit;
        sqlx::query(
            "UPDATE ih_upload_sessions SET chunks_bitmap = ?, received_size = received_size + ?
             WHERE id = ?",
        )
        .bind(&bitmap)
        .bind(len)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    find(db, id).await
}

/// Move from `from` to `to`, only if the session is in `from`. Returns
/// whether it moved — the guard that lets only one `complete` assemble.
pub async fn transition(
    db: &Db,
    id: &str,
    from: SessionStatus,
    to: SessionStatus,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("UPDATE ih_upload_sessions SET status = ? WHERE id = ? AND status = ?")
        .bind(to.as_str())
        .bind(id)
        .bind(from.as_str())
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// The session produced a file.
pub async fn finish(db: &Db, id: &str, file: FileId) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE ih_upload_sessions SET status = 'done', file_id = ? WHERE id = ?")
        .bind(file.0)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: &str) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM ih_upload_sessions WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Sessions past their expiry that are not finished, at most `limit`.
pub async fn expired(
    db: &Db,
    now: DateTime<Utc>,
    limit: u32,
) -> Result<Vec<UploadSession>, sqlx::Error> {
    let sql = format!(
        "SELECT {COLUMNS} FROM ih_upload_sessions
         WHERE expires_at <= ? AND status IN ('open', 'failed', 'assembling')
         ORDER BY expires_at LIMIT ?"
    );
    sqlx::query_as::<_, Row>(&sql)
        .bind(now)
        .bind(limit)
        .fetch_all(db)
        .await?
        .into_iter()
        .map(Row::into_session)
        .collect()
}

/// Mark a session expired, whatever state it was left in.
pub async fn mark_expired(db: &Db, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE ih_upload_sessions SET status = 'expired' WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(size: u64, chunk: u32) -> UploadSession {
        let total = chunks_for(size, chunk) as u32;
        UploadSession {
            id: "x".into(),
            user_id: None,
            filename: "f".into(),
            declared_size: size,
            received_size: 0,
            chunk_size: chunk,
            chunks_total: total,
            bitmap: vec![0; total.div_ceil(8) as usize],
            temp_path: String::new(),
            params: serde_json::Value::Null,
            status: SessionStatus::Open,
            file_id: None,
            expires_at: Utc::now(),
        }
    }

    #[test]
    fn chunks_cover_the_size_exactly() {
        assert_eq!(chunks_for(10, 4), 3);
        assert_eq!(chunks_for(8, 4), 2);
        assert_eq!(chunks_for(1, 4), 1);
        let s = session(10, 4);
        assert_eq!((s.chunk_len(0), s.chunk_len(1), s.chunk_len(2)), (4, 4, 2));
        assert_eq!(
            session(8, 4).chunk_len(1),
            4,
            "an exact fit has no short tail"
        );
    }

    #[test]
    fn the_bitmap_says_what_arrived() {
        let mut s = session(100, 10);
        s.bitmap[0] = 0b0000_0101;
        s.bitmap[1] = 0b0000_0010;
        assert_eq!(s.received(), vec![0, 2, 9]);
        assert_eq!(s.missing(), vec![1, 3, 4, 5, 6, 7, 8]);
    }
}
