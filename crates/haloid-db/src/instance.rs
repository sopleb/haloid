//! This installation's identity and its licence key: one row.

use crate::Db;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub instance_id: String,
    pub licence_key: Option<String>,
}

/// The instance row, made with `new_id` if this is the first start. Two
/// processes starting at once both try; the primary key lets one win, and
/// both read back the same row.
pub async fn ensure(db: &Db, new_id: &str) -> Result<Instance, sqlx::Error> {
    sqlx::query("INSERT IGNORE INTO ih_instance (id, instance_id) VALUES (1, ?)")
        .bind(new_id)
        .execute(db)
        .await?;
    let (instance_id, licence_key): (String, Option<String>) =
        sqlx::query_as("SELECT instance_id, licence_key FROM ih_instance WHERE id = 1")
            .fetch_one(db)
            .await?;
    Ok(Instance {
        instance_id,
        licence_key,
    })
}

/// The instance row, if the server has ever started.
pub async fn get(db: &Db) -> Result<Option<Instance>, sqlx::Error> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT instance_id, licence_key FROM ih_instance WHERE id = 1")
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(instance_id, licence_key)| Instance {
        instance_id,
        licence_key,
    }))
}

/// Install a licence key, or remove it with `None`.
pub async fn set_licence_key(db: &Db, key: Option<&str>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE ih_instance SET licence_key = ? WHERE id = 1")
        .bind(key)
        .execute(db)
        .await?;
    Ok(())
}
