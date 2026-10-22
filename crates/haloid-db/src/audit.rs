//! The audit log: what an administrator or moderator did, to what, and
//! from where. Written by the service for every administrative action;
//! never edited.

use crate::{Db, ip};
use chrono::{DateTime, Utc};
use haloid_core::ids::UserId;
use std::net::IpAddr;

#[derive(Debug, Clone)]
pub struct NewEntry<'a> {
    pub actor: Option<UserId>,
    /// `file.approve`, `settings.update`, `user.role`, …
    pub action: &'a str,
    /// `file`, `user`, `storage`, `settings`.
    pub target: Option<&'a str>,
    pub target_id: Option<u64>,
    pub detail: serde_json::Value,
    pub ip: Option<IpAddr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: u64,
    pub actor: Option<UserId>,
    pub actor_username: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub target_id: Option<u64>,
    pub detail: serde_json::Value,
    pub ip: Option<IpAddr>,
    pub created_at: DateTime<Utc>,
}

pub async fn record(db: &Db, entry: &NewEntry<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO ih_audit_log (actor_id, action, target, target_id, detail, ip)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(entry.actor.map(|u| u.0))
    .bind(entry.action)
    .bind(entry.target)
    .bind(entry.target_id)
    .bind(sqlx::types::Json(&entry.detail))
    .bind(entry.ip.map(ip::to_bytes))
    .execute(db)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Row {
    id: u64,
    actor_id: Option<u64>,
    username: Option<String>,
    action: String,
    target: Option<String>,
    target_id: Option<u64>,
    detail: Option<sqlx::types::Json<serde_json::Value>>,
    ip: Option<Vec<u8>>,
    created_at: DateTime<Utc>,
}

/// Newest first, before entry `before` when paging.
pub async fn list(db: &Db, before: Option<u64>, limit: u32) -> Result<Vec<Entry>, sqlx::Error> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT a.id, a.actor_id, u.username, a.action, a.target, a.target_id, a.detail, a.ip,
                a.created_at
         FROM ih_audit_log a LEFT JOIN ih_users u ON u.id = a.actor_id
         WHERE a.id < ?
         ORDER BY a.id DESC
         LIMIT ?",
    )
    .bind(before.unwrap_or(u64::MAX))
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Entry {
            id: r.id,
            actor: r.actor_id.map(UserId),
            actor_username: r.username,
            action: r.action,
            target: r.target,
            target_id: r.target_id,
            detail: r.detail.map(|d| d.0).unwrap_or(serde_json::Value::Null),
            ip: r.ip.as_deref().and_then(ip::from_bytes),
            created_at: r.created_at,
        })
        .collect())
}
