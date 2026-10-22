//! haloid-import — moving a Chevereto v4 installation into Haloid.
//!
//! This crate is the one place in Haloid's code allowed to name the format
//! it reads (see `CLAUDE.md`). It copies what a Chevereto database holds —
//! users and their password hashes, categories, albums, files, tags, likes
//! and storages — into Haloid's tables, and leaves the files where they are:
//! Chevereto's storage layout (date folders, `name.th.ext` and `name.md.ext`
//! beside each file) is Haloid's too, so the imported rows point at the same
//! bytes.
//!
//! What keeps old links working: every imported user, album and file keeps
//! its Chevereto public id in `legacy_id_encoded`, and the server redirects
//! Chevereto's `/image/…` and `/album/…` URLs to their Haloid pages. Direct
//! links to the files keep working when the imported storage is served at
//! the old URL.
//!
//! An import can be run again: rows already imported (by their legacy id)
//! are skipped, so an interrupted run picks up where it stopped. It writes
//! Haloid's tables directly, as a migration would, rather than through the
//! upload pipeline: the files exist already and must not be reprocessed.

pub mod legacy_id;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use haloid_db::Db;
use legacy_id::LegacyIds;
use sqlx::{MySqlPool, Row};
use std::collections::HashMap;
use std::path::PathBuf;

/// What to import, and from where.
#[derive(Debug, Clone)]
pub struct Options {
    /// Chevereto's table prefix. Default `chv_`.
    pub prefix: String,
    /// Where Chevereto's own files live (its `images/` folder), for files
    /// on no external storage. Required when there are any.
    pub files_dir: Option<PathBuf>,
    /// The URL those files were served at (`https://old.example.com/images`),
    /// so direct links keep working when the domain does.
    pub files_url: Option<String>,
    /// Look and report, change nothing.
    pub dry_run: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            prefix: "chv_".into(),
            files_dir: None,
            files_url: None,
            dry_run: false,
        }
    }
}

/// What an import did, or would do.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub users: u64,
    pub categories: u64,
    pub albums: u64,
    pub files: u64,
    pub tags: u64,
    pub likes: u64,
    pub storages: u64,
    /// Rows already imported by an earlier run.
    pub already: u64,
    /// Things that were not imported, and why.
    pub skipped: Vec<String>,
    /// Things the operator has to do: credentials to set, files to serve.
    pub todo: Vec<String>,
}

#[derive(Debug)]
pub enum ImportError {
    Source(sqlx::Error),
    Target(sqlx::Error),
    Missing(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(e) => write!(f, "reading the Chevereto database: {e}"),
            Self::Target(e) => write!(f, "writing the Haloid database: {e}"),
            Self::Missing(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for ImportError {}

fn src(e: sqlx::Error) -> ImportError {
    ImportError::Source(e)
}
fn dst(e: sqlx::Error) -> ImportError {
    ImportError::Target(e)
}

/// Chevereto stores GMT datetimes without a zone.
fn utc(at: NaiveDateTime) -> DateTime<Utc> {
    Utc.from_utc_datetime(&at)
}

fn new_id() -> String {
    haloid_id::generate_public_id()
}

/// The kind and type of a file by its extension, as Chevereto classifies
/// them.
fn kind_and_mime(extension: &str) -> Option<(&'static str, &'static str)> {
    Some(match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => ("image", "image/jpeg"),
        "png" => ("image", "image/png"),
        "gif" => ("image", "image/gif"),
        "webp" => ("image", "image/webp"),
        "avif" => ("image", "image/avif"),
        "heic" => ("image", "image/heic"),
        "heif" => ("image", "image/heif"),
        "bmp" => ("image", "image/bmp"),
        "mp4" => ("video", "video/mp4"),
        "webm" => ("video", "video/webm"),
        "mov" => ("video", "video/quicktime"),
        "mp3" => ("audio", "audio/mpeg"),
        "m4a" => ("audio", "audio/mp4"),
        "wav" => ("audio", "audio/wav"),
        _ => return None,
    })
}

/// Chevereto's settings for its ids: `crypt_salt` and `id_padding`, in the
/// `variables` table on v4 and in `settings` before it.
async fn id_settings(source: &MySqlPool, prefix: &str) -> Result<LegacyIds, ImportError> {
    let mut found: HashMap<String, String> = HashMap::new();
    for (table, name, value) in [
        ("settings", "setting_name", "setting_value"),
        ("variables", "variable_name", "variable_value"),
    ] {
        let sql = format!(
            "SELECT {name}, {value} FROM {prefix}{table} WHERE {name} IN ('crypt_salt', 'id_padding')"
        );
        // A missing table is fine: the other one has them.
        if let Ok(rows) = sqlx::query(&sql).fetch_all(source).await {
            for row in rows {
                let key: String = row.try_get(0).map_err(src)?;
                let value: Option<String> = row.try_get(1).map_err(src)?;
                if let Some(value) = value {
                    found.insert(key, value);
                }
            }
        }
    }
    let salt = found.get("crypt_salt").cloned().unwrap_or_default();
    if salt.is_empty() {
        tracing::warn!("Chevereto's crypt_salt was not found; old ids will not match its links");
    }
    let padding = found
        .get("id_padding")
        .and_then(|p| p.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Ok(LegacyIds::new(&salt, padding))
}

/// The Haloid row already imported for a legacy id, if any.
async fn imported(db: &Db, table: &str, legacy: &str) -> Result<Option<u64>, ImportError> {
    let sql = format!("SELECT id FROM {table} WHERE legacy_id_encoded = ?");
    sqlx::query_scalar(&sql)
        .bind(legacy)
        .fetch_optional(db)
        .await
        .map_err(dst)
}

/// Import from `source` into `target`.
pub async fn run(
    source: &MySqlPool,
    target: &Db,
    options: &Options,
) -> Result<Report, ImportError> {
    let prefix = &options.prefix;
    let ids = id_settings(source, prefix).await?;
    let mut report = Report::default();
    let dry = options.dry_run;

    // Storages: Chevereto's own folder, and its external ones. In a dry run
    // nothing is written, so a stand-in id marks the ones that would be.
    let mut storages: HashMap<Option<u64>, u64> = HashMap::new();
    let local_files: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {prefix}images WHERE image_storage_id IS NULL"
    ))
    .fetch_one(source)
    .await
    .map_err(src)?;
    if local_files > 0 {
        let Some(dir) = &options.files_dir else {
            return Err(ImportError::Missing(format!(
                "{local_files} files are in Chevereto's own images folder: give its path (--files-dir)"
            )));
        };
        let url = options.files_url.clone().unwrap_or_default();
        if url.is_empty() {
            report.todo.push(
                "set the imported storage's URL prefix (Admin → Storages) to where its files are served"
                    .into(),
            );
        }
        report.storages += 1;
        let id = if dry {
            0
        } else {
            let config = serde_json::json!({ "base_path": dir }).to_string();
            upsert_storage(target, "imported", "local", &config, &url).await?
        };
        storages.insert(None, id);
    }
    let rows = sqlx::query(&format!(
        "SELECT s.storage_id, s.storage_name, s.storage_url, s.storage_bucket, s.storage_region,
                s.storage_server, s.storage_use_path_style_endpoint, a.storage_api_type
         FROM {prefix}storages s JOIN {prefix}storage_apis a ON a.storage_api_id = s.storage_api_id"
    ))
    .fetch_all(source)
    .await
    .unwrap_or_default();
    for row in rows {
        let id: u32 = row.try_get("storage_id").map_err(src)?;
        let name: String = row.try_get("storage_name").map_err(src)?;
        let url: String = row.try_get("storage_url").map_err(src)?;
        let bucket: Option<String> = row.try_get("storage_bucket").map_err(src)?;
        let region: Option<String> = row.try_get("storage_region").map_err(src)?;
        let server: Option<String> = row.try_get("storage_server").map_err(src)?;
        let path_style: u8 = row.try_get("storage_use_path_style_endpoint").unwrap_or(0);
        let api: String = row.try_get("storage_api_type").map_err(src)?;
        let slug: String = format!("imported-{id}");
        let (kind, config) = match api.as_str() {
            "local" => (
                "local",
                serde_json::json!({ "base_path": bucket.unwrap_or_default() }),
            ),
            "s3" | "s3compatible" | "alibaba" | "backblaze" | "wasabi" | "cloudflare" | "r2" => {
                report.todo.push(format!(
                    "storage {name:?} is S3-compatible: set HALOID_STORAGE_{}_ACCESS_KEY and _SECRET_KEY (Haloid Pro)",
                    slug.to_uppercase().replace('-', "_")
                ));
                (
                    "s3",
                    serde_json::json!({
                        "bucket": bucket.unwrap_or_default(),
                        "region": region,
                        "endpoint": server,
                        "path_style": path_style == 1,
                    }),
                )
            }
            other => {
                report.skipped.push(format!(
                    "storage {name:?}: Haloid has no {other} storage; its files are skipped"
                ));
                continue;
            }
        };
        report.storages += 1;
        let haloid = if dry {
            0
        } else {
            upsert_storage(target, &slug, kind, &config.to_string(), &url).await?
        };
        storages.insert(Some(u64::from(id)), haloid);
    }

    // Categories, by their URL key.
    let mut categories: HashMap<u64, u64> = HashMap::new();
    let rows = sqlx::query(&format!(
        "SELECT category_id, category_name, category_url_key, category_description FROM {prefix}categories"
    ))
    .fetch_all(source)
    .await
    .unwrap_or_default();
    for row in rows {
        let id: u32 = row.try_get(0).map_err(src)?;
        let name: String = row.try_get(1).map_err(src)?;
        let key: String = row.try_get(2).map_err(src)?;
        let description: Option<String> = row.try_get(3).map_err(src)?;
        report.categories += 1;
        if dry {
            continue;
        }
        sqlx::query(
            "INSERT INTO ih_categories (name, url_key, description) VALUES (?, ?, ?)
             ON DUPLICATE KEY UPDATE id = LAST_INSERT_ID(id)",
        )
        .bind(&name)
        .bind(&key)
        .bind(&description)
        .execute(target)
        .await
        .map_err(dst)?;
        let haloid: u64 = sqlx::query_scalar("SELECT id FROM ih_categories WHERE url_key = ?")
            .bind(&key)
            .fetch_one(target)
            .await
            .map_err(dst)?;
        categories.insert(u64::from(id), haloid);
    }

    // Users, with their newest password hash. Chevereto's are bcrypt, which
    // Haloid accepts at login and replaces with argon2id there.
    let mut users: HashMap<u64, u64> = HashMap::new();
    let rows = sqlx::query(&format!(
        "SELECT u.user_id, u.user_username, u.user_name, u.user_email, u.user_status, u.user_is_admin,
                u.user_is_manager, u.user_is_private, u.user_timezone, u.user_date_gmt,
                (SELECT p.login_password_hash FROM {prefix}login_passwords p
                  WHERE p.login_password_user_id = u.user_id
                  ORDER BY p.login_password_date_gmt DESC LIMIT 1) AS hash
         FROM {prefix}users u ORDER BY u.user_id"
    ))
    .fetch_all(source)
    .await
    .map_err(src)?;
    for row in rows {
        let id: u32 = row.try_get("user_id").map_err(src)?;
        let username: String = row.try_get("user_username").map_err(src)?;
        let legacy = ids.encode(u64::from(id));
        if let Some(existing) = imported(target, "ih_users", &legacy).await? {
            users.insert(u64::from(id), existing);
            report.already += 1;
            continue;
        }
        if username.is_empty() || username.chars().count() > 64 {
            report
                .skipped
                .push(format!("user {username:?}: not a username Haloid can hold"));
            continue;
        }
        let taken: Option<u64> =
            sqlx::query_scalar("SELECT id FROM ih_users WHERE username_lower = ?")
                .bind(username.to_lowercase())
                .fetch_optional(target)
                .await
                .map_err(dst)?;
        if taken.is_some() {
            report.skipped.push(format!(
                "user {username:?}: that username is taken here already"
            ));
            continue;
        }
        report.users += 1;
        if dry {
            continue;
        }
        let email: Option<String> = row.try_get("user_email").map_err(src)?;
        let status: String = row.try_get("user_status").map_err(src)?;
        let status = match status.as_str() {
            "banned" => "banned",
            "valid" => "valid",
            _ => "awaiting_confirmation",
        };
        let admin: u8 = row.try_get("user_is_admin").map_err(src)?;
        let manager: u8 = row.try_get("user_is_manager").map_err(src)?;
        let role = if admin == 1 {
            "admin"
        } else if manager == 1 {
            "manager"
        } else {
            "user"
        };
        let created: NaiveDateTime = row.try_get("user_date_gmt").map_err(src)?;
        let hash: Option<String> = row.try_get("hash").map_err(src)?;
        let done = sqlx::query(
            "INSERT INTO ih_users
               (id_encoded, username, username_lower, email, email_lower, password_hash, role, status,
                display_name, timezone, is_private, legacy_id_encoded, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(new_id())
        .bind(&username)
        .bind(username.to_lowercase())
        .bind(&email)
        .bind(email.as_ref().map(|e| e.to_lowercase()))
        .bind(&hash)
        .bind(role)
        .bind(status)
        .bind(row.try_get::<Option<String>, _>("user_name").map_err(src)?)
        .bind(row.try_get::<String, _>("user_timezone").map_err(src)?)
        .bind(row.try_get::<u8, _>("user_is_private").map_err(src)?)
        .bind(&legacy)
        .bind(utc(created))
        .execute(target)
        .await
        .map_err(dst)?;
        users.insert(u64::from(id), done.last_insert_id());
    }

    // Albums: first all of them, then their parents, which may come later.
    let mut albums: HashMap<u64, u64> = HashMap::new();
    let mut parents: Vec<(u64, u64)> = Vec::new();
    let mut covers: Vec<(u64, u64)> = Vec::new();
    let rows = sqlx::query(&format!(
        "SELECT album_id, album_name, album_user_id, album_date_gmt, album_privacy, album_password,
                album_description, album_views, album_cover_id, album_parent_id
         FROM {prefix}albums ORDER BY album_id"
    ))
    .fetch_all(source)
    .await
    .map_err(src)?;
    for row in rows {
        let id: u32 = row.try_get("album_id").map_err(src)?;
        let legacy = ids.encode(u64::from(id));
        let owner: Option<u32> = row.try_get("album_user_id").map_err(src)?;
        if let Some(existing) = imported(target, "ih_albums", &legacy).await? {
            albums.insert(u64::from(id), existing);
            report.already += 1;
            continue;
        }
        let owner = match owner {
            Some(o) => match users.get(&u64::from(o)) {
                Some(u) => Some(*u),
                None if dry => None,
                None => {
                    report
                        .skipped
                        .push(format!("album {id}: its owner was not imported"));
                    continue;
                }
            },
            None => None,
        };
        report.albums += 1;
        if dry {
            continue;
        }
        let privacy: Option<String> = row.try_get("album_privacy").map_err(src)?;
        let password: Option<String> = row.try_get("album_password").map_err(src)?;
        let (privacy, hash) = match privacy.as_deref() {
            Some("password") => match password.filter(|p| !p.is_empty()) {
                // Chevereto keeps album passwords as written; Haloid keeps
                // only a hash.
                Some(p) => (
                    "password",
                    Some(
                        haloid_service::auth::secrets::hash_password(&p)
                            .map_err(|e| ImportError::Missing(e.to_string()))?,
                    ),
                ),
                None => ("private", None),
            },
            Some("private") | Some("custom") => ("private", None),
            Some("private_but_link") => ("unlisted", None),
            _ => ("public", None),
        };
        let name: String = row.try_get("album_name").map_err(src)?;
        let created: NaiveDateTime = row.try_get("album_date_gmt").map_err(src)?;
        let done = sqlx::query(
            "INSERT INTO ih_albums
               (id_encoded, user_id, name, description, privacy, password_hash, view_count,
                legacy_id_encoded, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(new_id())
        .bind(owner)
        .bind(name.chars().take(190).collect::<String>())
        .bind(
            row.try_get::<Option<String>, _>("album_description")
                .map_err(src)?,
        )
        .bind(privacy)
        .bind(hash)
        .bind(row.try_get::<u32, _>("album_views").map_err(src)?)
        .bind(&legacy)
        .bind(utc(created))
        .execute(target)
        .await
        .map_err(dst)?;
        let haloid = done.last_insert_id();
        albums.insert(u64::from(id), haloid);
        if let Some(parent) = row
            .try_get::<Option<u32>, _>("album_parent_id")
            .map_err(src)?
        {
            parents.push((haloid, u64::from(parent)));
        }
        if let Some(cover) = row
            .try_get::<Option<u32>, _>("album_cover_id")
            .map_err(src)?
        {
            covers.push((haloid, u64::from(cover)));
        }
    }
    for (album, parent) in parents {
        if let Some(parent) = albums.get(&parent) {
            sqlx::query("UPDATE ih_albums SET parent_id = ? WHERE id = ?")
                .bind(parent)
                .bind(album)
                .execute(target)
                .await
                .map_err(dst)?;
        }
    }

    // Files, in batches, in id order (which is also their album order).
    let mut files: HashMap<u64, u64> = HashMap::new();
    let mut after: u32 = 0;
    loop {
        let rows = sqlx::query(&format!(
            "SELECT image_id, image_name, image_extension, image_size, image_width, image_height,
                    image_date, image_date_gmt, image_title, image_description, image_nsfw, image_user_id,
                    image_album_id, image_uploader_ip, image_storage_mode, image_path, image_storage_id,
                    image_checksum, image_source_checksum, image_original_filename, image_original_exifdata,
                    image_views, image_category_id, image_chain, image_thumb_size, image_medium_size,
                    image_frame_size, image_expiration_date_gmt, image_likes, image_is_animated,
                    image_is_approved, image_is_360, image_duration
             FROM {prefix}images WHERE image_id > ? ORDER BY image_id LIMIT 500"
        ))
        .bind(after)
        .fetch_all(source)
        .await
        .map_err(src)?;
        let Some(last) = rows.last() else { break };
        after = last.try_get("image_id").map_err(src)?;
        for row in rows {
            import_file(
                &row,
                target,
                &ids,
                &storages,
                &users,
                &albums,
                &categories,
                &mut files,
                &mut report,
                dry,
            )
            .await?;
        }
    }

    // Album covers, now the files exist.
    for (album, cover) in covers {
        if let Some(file) = files.get(&cover) {
            sqlx::query("UPDATE ih_albums SET cover_file_id = ? WHERE id = ?")
                .bind(file)
                .bind(album)
                .execute(target)
                .await
                .map_err(dst)?;
        }
    }

    // Tags, in the order Chevereto linked them.
    let rows = sqlx::query(&format!(
        "SELECT tf.tag_file_file_id, t.tag_name FROM {prefix}tags_files tf
         JOIN {prefix}tags t ON t.tag_id = tf.tag_file_tag_id
         ORDER BY tf.tag_file_file_id, t.tag_id"
    ))
    .fetch_all(source)
    .await
    .unwrap_or_default();
    let mut by_file: Vec<(u64, Vec<String>)> = Vec::new();
    for row in rows {
        let file: u32 = row.try_get(0).map_err(src)?;
        let name: String = row.try_get(1).map_err(src)?;
        match by_file.last_mut() {
            Some((f, names)) if *f == u64::from(file) => names.push(name),
            _ => by_file.push((u64::from(file), vec![name])),
        }
    }
    for (file, names) in by_file {
        let Some(haloid) = files.get(&file) else {
            continue;
        };
        report.tags += names.len() as u64;
        if dry {
            continue;
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        haloid_db::tags::set_for_file(target, haloid_core::ids::FileId(*haloid), &refs)
            .await
            .map_err(dst)?;
    }

    // Likes of files, by users that came across.
    let rows = sqlx::query(&format!(
        "SELECT like_user_id, like_content_id, like_date_gmt FROM {prefix}likes
         WHERE like_content_type = 'image' AND like_user_id IS NOT NULL"
    ))
    .fetch_all(source)
    .await
    .unwrap_or_default();
    for row in rows {
        let user: u32 = row.try_get(0).map_err(src)?;
        let file: u32 = row.try_get(1).map_err(src)?;
        let (Some(user), Some(file)) = (users.get(&u64::from(user)), files.get(&u64::from(file)))
        else {
            continue;
        };
        report.likes += 1;
        if dry {
            continue;
        }
        let at: NaiveDateTime = row.try_get(2).map_err(src)?;
        sqlx::query("INSERT IGNORE INTO ih_likes (user_id, file_id, created_at) VALUES (?, ?, ?)")
            .bind(user)
            .bind(file)
            .bind(utc(at))
            .execute(target)
            .await
            .map_err(dst)?;
    }

    if !dry {
        recount(target).await?;
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn import_file(
    row: &sqlx::mysql::MySqlRow,
    target: &Db,
    ids: &LegacyIds,
    storages: &HashMap<Option<u64>, u64>,
    users: &HashMap<u64, u64>,
    albums: &HashMap<u64, u64>,
    categories: &HashMap<u64, u64>,
    files: &mut HashMap<u64, u64>,
    report: &mut Report,
    dry: bool,
) -> Result<(), ImportError> {
    let id: u32 = row.try_get("image_id").map_err(src)?;
    let legacy = ids.encode(u64::from(id));
    if let Some(existing) = imported(target, "ih_files", &legacy).await? {
        files.insert(u64::from(id), existing);
        report.already += 1;
        return Ok(());
    }
    let name: String = row.try_get("image_name").map_err(src)?;
    let extension: String = row.try_get("image_extension").map_err(src)?;
    let Some((kind, mime)) = kind_and_mime(&extension) else {
        report.skipped.push(format!(
            "file {id}: Haloid does not take .{extension} files"
        ));
        return Ok(());
    };
    let storage_id: Option<u32> = row.try_get("image_storage_id").map_err(src)?;
    let storage = storages.get(&storage_id.map(u64::from)).copied();
    if storage.is_none() {
        report
            .skipped
            .push(format!("file {id}: its storage was not imported"));
        return Ok(());
    }
    // Where the file sits in its storage. Chevereto's `datefolder` is the
    // upload's local date; `path` is a folder of its own.
    let mode: String = row.try_get("image_storage_mode").map_err(src)?;
    let local_date: NaiveDateTime = row.try_get("image_date").map_err(src)?;
    let folder: Option<String> = row.try_get("image_path").map_err(src)?;
    let (storage_mode, key) = match mode.as_str() {
        "datefolder" => (
            "datefolder",
            format!("{}/{name}.{extension}", local_date.format("%Y/%m/%d")),
        ),
        "path" => (
            "path",
            format!(
                "{}/{name}.{extension}",
                folder.unwrap_or_default().trim_matches('/')
            )
            .trim_start_matches('/')
            .to_string(),
        ),
        _ => ("direct", format!("{name}.{extension}")),
    };
    let owner: Option<u32> = row.try_get("image_user_id").map_err(src)?;
    let owner = match owner {
        Some(o) => match users.get(&u64::from(o)) {
            Some(u) => Some(*u),
            None if dry => None,
            None => {
                report
                    .skipped
                    .push(format!("file {id}: its owner was not imported"));
                return Ok(());
            }
        },
        None => None,
    };
    report.files += 1;
    if dry {
        return Ok(());
    }
    let album = row
        .try_get::<Option<u32>, _>("image_album_id")
        .map_err(src)?
        .and_then(|a| albums.get(&u64::from(a)).copied());
    let category = row
        .try_get::<Option<u32>, _>("image_category_id")
        .map_err(src)?
        .and_then(|c| categories.get(&u64::from(c)).copied());
    let exif: Option<String> = row
        .try_get::<Option<String>, _>("image_original_exifdata")
        .map_err(src)?
        .filter(|e| serde_json::from_str::<serde_json::Value>(e).is_ok());
    let ip: Option<String> = row.try_get("image_uploader_ip").map_err(src)?;
    let ip_bytes = ip
        .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
        .map(|ip| match ip {
            std::net::IpAddr::V4(v4) => v4.octets().to_vec(),
            std::net::IpAddr::V6(v6) => v6.octets().to_vec(),
        });
    let created: NaiveDateTime = row.try_get("image_date_gmt").map_err(src)?;
    let expires: Option<NaiveDateTime> = row.try_get("image_expiration_date_gmt").map_err(src)?;
    let duration: u32 = row.try_get("image_duration").map_err(src)?;
    let size = |column: &str| -> Result<Option<u64>, ImportError> {
        let v: u32 = row.try_get(column).map_err(src)?;
        Ok((v > 0).then_some(u64::from(v)))
    };
    let done = sqlx::query(
        "INSERT INTO ih_files
           (id_encoded, kind, state, name, extension, mime, original_filename, title, description, size,
            width, height, duration_ms, is_animated, chain, thumb_size, medium_size, frame_size,
            checksum, source_checksum, storage_id, storage_mode, storage_path, user_id, album_id,
            album_position, category_id, nsfw, is_approved, is_360, view_count, like_count, exif,
            uploader_ip, expires_at, legacy_id_encoded, created_at)
         VALUES (?, ?, 'ready', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(kind)
    .bind(name.chars().take(190).collect::<String>())
    .bind(extension.to_ascii_lowercase())
    .bind(mime)
    .bind(row.try_get::<String, _>("image_original_filename").map_err(src)?)
    .bind(row.try_get::<Option<String>, _>("image_title").map_err(src)?)
    .bind(row.try_get::<Option<String>, _>("image_description").map_err(src)?)
    .bind(row.try_get::<u64, _>("image_size").map_err(src)?)
    .bind(row.try_get::<u32, _>("image_width").map_err(src)?)
    .bind(row.try_get::<u32, _>("image_height").map_err(src)?)
    .bind((duration > 0).then(|| duration.saturating_mul(1000)))
    .bind(row.try_get::<u8, _>("image_is_animated").map_err(src)?)
    .bind(row.try_get::<u8, _>("image_chain").map_err(src)?)
    .bind(size("image_thumb_size")?)
    .bind(size("image_medium_size")?)
    .bind(size("image_frame_size")?)
    .bind(row.try_get::<String, _>("image_checksum").map_err(src)?)
    .bind(row.try_get::<Option<String>, _>("image_source_checksum").map_err(src)?)
    .bind(storage)
    .bind(storage_mode)
    .bind(&key)
    .bind(owner)
    .bind(album)
    .bind(album.map(|_| id))
    .bind(category)
    .bind(row.try_get::<u8, _>("image_nsfw").map_err(src)?)
    .bind(row.try_get::<u8, _>("image_is_approved").map_err(src)?)
    .bind(row.try_get::<u8, _>("image_is_360").map_err(src)?)
    .bind(row.try_get::<u32, _>("image_views").map_err(src)?)
    .bind(row.try_get::<u32, _>("image_likes").map_err(src)?)
    .bind(exif)
    .bind(ip_bytes)
    .bind(expires.map(utc))
    .bind(&legacy)
    .bind(utc(created))
    .execute(target)
    .await
    .map_err(dst)?;
    files.insert(u64::from(id), done.last_insert_id());
    Ok(())
}

async fn upsert_storage(
    target: &Db,
    name: &str,
    kind: &str,
    config: &str,
    url: &str,
) -> Result<u64, ImportError> {
    if let Some(id) = sqlx::query_scalar::<_, u64>("SELECT id FROM ih_storages WHERE name = ?")
        .bind(name)
        .fetch_optional(target)
        .await
        .map_err(dst)?
    {
        return Ok(id);
    }
    // Not active: imported storages serve their files and take no new
    // uploads until an administrator says so.
    let done = sqlx::query(
        "INSERT INTO ih_storages (name, kind, config, url_prefix, path_mode, is_active, priority)
         VALUES (?, ?, ?, ?, 'datefolder', 0, 0)",
    )
    .bind(name)
    .bind(kind)
    .bind(config)
    .bind(url.trim_end_matches('/'))
    .execute(target)
    .await
    .map_err(dst)?;
    Ok(done.last_insert_id())
}

/// The denormalised counters, from what is now there.
async fn recount(target: &Db) -> Result<(), ImportError> {
    for sql in [
        "UPDATE ih_users u SET
           file_count = (SELECT COUNT(*) FROM ih_files f WHERE f.user_id = u.id AND f.deleted_at IS NULL),
           disk_used = (SELECT COALESCE(SUM(f.size + COALESCE(f.thumb_size,0) + COALESCE(f.medium_size,0)
                          + COALESCE(f.frame_size,0)), 0) FROM ih_files f WHERE f.user_id = u.id),
           album_count = (SELECT COUNT(*) FROM ih_albums a WHERE a.user_id = u.id)",
        "UPDATE ih_albums a SET file_count = (SELECT COUNT(*) FROM ih_files f
           WHERE f.album_id = a.id AND f.deleted_at IS NULL)",
        "UPDATE ih_storages s SET
           file_count = (SELECT COUNT(*) FROM ih_files f WHERE f.storage_id = s.id),
           used_bytes = (SELECT COALESCE(SUM(f.size + COALESCE(f.thumb_size,0) + COALESCE(f.medium_size,0)
                          + COALESCE(f.frame_size,0)), 0) FROM ih_files f WHERE f.storage_id = s.id)",
        "UPDATE ih_categories c SET file_count = (SELECT COUNT(*) FROM ih_files f
           WHERE f.category_id = c.id AND f.deleted_at IS NULL)",
        "UPDATE ih_files f SET like_count = GREATEST(f.like_count,
           (SELECT COUNT(*) FROM ih_likes l WHERE l.file_id = f.id))",
    ] {
        sqlx::query(sql).execute(target).await.map_err(dst)?;
    }
    Ok(())
}
