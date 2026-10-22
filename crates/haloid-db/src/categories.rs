//! The categories repository: a short, administrator-curated list.

use crate::Db;
use crate::error::{DbError, unique_violation};
use haloid_core::ids::CategoryId;
use haloid_core::taxonomy::Category;

/// Every editable field, for creating or replacing a category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryDetails {
    pub name: String,
    /// Unique, case-insensitively.
    pub url_key: String,
    pub description: Option<String>,
    /// Lower sorts first.
    pub sort_order: i32,
}

const COLUMNS: &str = "c.id, c.name, c.url_key, c.description, c.file_count, c.sort_order";

#[derive(sqlx::FromRow)]
struct CategoryRow {
    id: u32,
    name: String,
    url_key: String,
    description: Option<String>,
    file_count: u32,
    sort_order: i32,
}

impl From<CategoryRow> for Category {
    fn from(row: CategoryRow) -> Self {
        Category {
            id: CategoryId(row.id),
            name: row.name,
            url_key: row.url_key,
            description: row.description,
            file_count: row.file_count,
            sort_order: row.sort_order,
        }
    }
}

fn conflict(error: sqlx::Error) -> DbError {
    if unique_violation(&error).as_deref() == Some("uq_categories_url_key") {
        DbError::Conflict("url_key")
    } else {
        error.into()
    }
}

pub async fn create(db: &Db, details: &CategoryDetails) -> Result<Category, DbError> {
    let done = sqlx::query(
        "INSERT INTO ih_categories (name, url_key, description, sort_order) VALUES (?, ?, ?, ?)",
    )
    .bind(&details.name)
    .bind(&details.url_key)
    .bind(&details.description)
    .bind(details.sort_order)
    .execute(db)
    .await
    .map_err(conflict)?;
    let id = u32::try_from(done.last_insert_id()).map_err(|_| sqlx::Error::RowNotFound)?;
    find_by_id(db, CategoryId(id))
        .await?
        .ok_or(DbError::Sqlx(sqlx::Error::RowNotFound))
}

/// Every category, in display order.
pub async fn list(db: &Db) -> Result<Vec<Category>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_categories c ORDER BY c.sort_order, c.name, c.id");
    let rows: Vec<CategoryRow> = sqlx::query_as(&sql).fetch_all(db).await?;
    Ok(rows.into_iter().map(Category::from).collect())
}

pub async fn find_by_id(db: &Db, id: CategoryId) -> Result<Option<Category>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_categories c WHERE c.id = ?");
    Ok(sqlx::query_as::<_, CategoryRow>(&sql)
        .bind(id.0)
        .fetch_optional(db)
        .await?
        .map(Category::from))
}

pub async fn find_by_url_key(db: &Db, url_key: &str) -> Result<Option<Category>, sqlx::Error> {
    let sql = format!("SELECT {COLUMNS} FROM ih_categories c WHERE c.url_key = ?");
    Ok(sqlx::query_as::<_, CategoryRow>(&sql)
        .bind(url_key)
        .fetch_optional(db)
        .await?
        .map(Category::from))
}

/// Returns whether the category exists.
pub async fn update(db: &Db, id: CategoryId, details: &CategoryDetails) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE ih_categories SET name = ?, url_key = ?, description = ?, sort_order = ?
         WHERE id = ?",
    )
    .bind(&details.name)
    .bind(&details.url_key)
    .bind(&details.description)
    .bind(details.sort_order)
    .bind(id.0)
    .execute(db)
    .await
    .map_err(conflict)?;
    Ok(done.rows_affected() > 0)
}

/// Remove a category. Its files stay, uncategorised (the foreign key's
/// `ON DELETE SET NULL`). Returns whether it existed.
pub async fn delete(db: &Db, id: CategoryId) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM ih_categories WHERE id = ?")
        .bind(id.0)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}
