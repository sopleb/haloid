//! The upload pipeline: the one function both API surfaces call.
//!
//! Twelve steps, in the order `design.md` gives them. The legacy and v2 APIs
//! build the same [`UploadCommand`] and call [`Uploads::upload`]; nothing
//! else may run any part of this. That is what keeps the two surfaces from
//! drifting apart.
//!
//! Bytes are ingested by this module, not by the API layer, so the size
//! limit is enforced in one place and while streaming: a 10 GB body costs
//! at most the limit in disk before it is refused. Every temporary file is
//! removed when the upload ends, however it ends.

use crate::settings::SettingsHandle;
use base64::Engine;
use chrono::{Duration as ChronoDuration, Utc};
use haloid_core::DomainError;
use haloid_core::auth::Actor;
use haloid_core::files::{Album, MediaFile};
use haloid_core::ids::{CategoryId, FileId};
use haloid_core::media::{Derivative, MediaKind};
use haloid_core::processor::{DerivativeFile, DerivativePlan, MediaProcessor, Watermark};
use haloid_core::storage::{PutBody, StorageKind};
use haloid_core::taxonomy::Tag;
use haloid_db::{Db, albums, categories, files, moderation, storages, tags, users};
use haloid_storage::keys::{self, KeyParts};
use haloid_storage::pool::StoragePool;
use md5::Md5;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// Where the bytes come from.
pub enum UploadSource {
    /// A request body, multipart field or anything else that reads.
    Stream {
        reader: Box<dyn AsyncRead + Send + Unpin>,
        filename: Option<String>,
    },
    /// Base64 text, with or without a `data:…;base64,` prefix and with any
    /// whitespace curl or ShareX put in it.
    Base64 {
        data: String,
        filename: Option<String>,
    },
    /// A URL to fetch. Refused until the SSRF-hardened fetcher exists.
    Url(String),
}

/// Everything an upload can ask for. Both API surfaces build this.
pub struct UploadCommand {
    pub actor: Actor,
    pub source: UploadSource,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// An album's public id.
    pub album: Option<String>,
    pub category: Option<CategoryId>,
    /// `None` takes the instance default.
    pub nsfw: Option<bool>,
    /// Already parsed; capped here at the instance maximum.
    pub expiration: Option<Duration>,
    /// Shrink the served file to this width, keeping the original.
    pub width: Option<u32>,
    pub uploader_ip: Option<IpAddr>,
}

/// Public URLs of a stored upload.
#[derive(Debug, Clone, PartialEq)]
pub struct FileUrls {
    /// The viewer page.
    pub viewer: String,
    /// The one-shot delete link, for whoever uploaded it.
    pub delete: Option<String>,
    /// Each stored derivative, by its legacy key (`image`, `thumb`, …).
    pub derivatives: BTreeMap<&'static str, String>,
}

/// The rich result: a superset of what either API returns.
#[derive(Debug, Clone)]
pub struct UploadedFile {
    pub file: MediaFile,
    pub tags: Vec<Tag>,
    pub album: Option<Album>,
    /// The owner's username; `None` for a guest's upload.
    pub owner: Option<String>,
    pub urls: FileUrls,
    /// Whether this is an earlier upload of the same bytes, returned instead
    /// of storing them twice.
    pub duplicate: bool,
    /// The captured EXIF JSON, for the legacy `original_exifdata`.
    pub exif: Option<String>,
    /// Where the bytes live, for the legacy `file.resource.type`.
    pub storage_kind: Option<StorageKind>,
    /// An argon2 hash of the delete token, present whenever the delete
    /// link is. The legacy API exposes one because upstream does (it leaks
    /// its own); the hash is of a random 128-bit token, so it reveals
    /// nothing practical. Never stored.
    pub delete_hash: Option<String>,
}

pub struct Uploads {
    db: Db,
    settings: SettingsHandle,
    media: Arc<dyn MediaProcessor>,
    storages: Arc<StoragePool>,
    temp_dir: PathBuf,
    fetcher: crate::fetch::Fetcher,
    limiter: crate::auth::limits::RateLimiter,
    metrics: Arc<crate::metrics::Metrics>,
    licensing: Option<Arc<crate::licensing::Licensing>>,
    /// Recent viewers, so a reload is not a second view.
    viewed: std::sync::Mutex<std::collections::HashMap<(u64, String), std::time::Instant>>,
}

/// One viewer counts once per file in this long.
const VIEW_WINDOW: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// Past this many remembered views, expired ones are swept out.
const VIEWS_REMEMBERED: usize = 50_000;

/// Temporary files, removed on drop whatever happened.
#[derive(Default)]
struct Temps(Vec<PathBuf>);

impl Temps {
    fn track(&mut self, path: PathBuf) -> PathBuf {
        self.0.push(path.clone());
        path
    }
}

impl Drop for Temps {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct Ingested {
    path: PathBuf,
    filename: Option<String>,
    source_md5: String,
    sha256: String,
}

fn random_base62(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| haloid_id::ALPHABET[rng.random_range(0..haloid_id::ALPHABET.len())] as char)
        .collect()
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

fn io(error: std::io::Error) -> DomainError {
    DomainError::Internal(Box::new(error))
}

/// The keys an upload's objects are stored under, by derivative.
pub fn object_keys(file: &MediaFile) -> Result<Vec<(Derivative, String)>, DomainError> {
    let (derivative_extension, _) = file.kind.derivative_format(&file.extension);
    let mut found = Vec::new();
    for derivative in file.chain.iter() {
        let extension = match derivative {
            Derivative::Image => file.extension.as_str(),
            Derivative::Original => match file.original_extension.as_deref() {
                Some(extension) => extension,
                None => continue,
            },
            Derivative::Thumb | Derivative::Medium | Derivative::Frame => derivative_extension,
        };
        let key = keys::sibling_key(&file.storage_path, derivative, extension)?;
        found.push((derivative, key));
    }
    Ok(found)
}

impl Uploads {
    /// `temp_dir` must be on the same filesystem as local storage for
    /// uploads to be moved into place rather than copied.
    pub fn new(
        db: Db,
        settings: SettingsHandle,
        media: Arc<dyn MediaProcessor>,
        storages: Arc<StoragePool>,
        temp_dir: PathBuf,
    ) -> Self {
        Self {
            db,
            settings,
            media,
            storages,
            temp_dir,
            fetcher: crate::fetch::Fetcher::default(),
            // The rate is read from the settings on every check.
            limiter: crate::auth::limits::RateLimiter::new(1, std::time::Duration::from_secs(1)),
            metrics: Arc::default(),
            licensing: None,
            viewed: std::sync::Mutex::default(),
        }
    }

    /// Hold uploads to this licence: audio is Pro. Without one, Community.
    pub fn with_licensing(mut self, licensing: Arc<crate::licensing::Licensing>) -> Self {
        self.licensing = Some(licensing);
        self
    }

    fn entitlements(&self) -> haloid_core::entitlements::Entitlements {
        self.licensing
            .as_ref()
            .map_or(haloid_core::entitlements::Entitlements::COMMUNITY, |l| {
                l.entitlements(Utc::now())
            })
    }

    /// Count into these metrics rather than a private set.
    pub fn with_metrics(mut self, metrics: Arc<crate::metrics::Metrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Hold an actor to the hourly upload rate: guests by address, users by
    /// account, moderators and administrators not at all. Shared by both
    /// APIs and by chunked sessions, so none of them is a way around it.
    pub(crate) fn check_rate(
        &self,
        what: &str,
        actor: Actor,
        ip: Option<std::net::IpAddr>,
    ) -> Result<(), DomainError> {
        let settings = self.settings.get();
        let (key, per_hour) = match actor {
            Actor::Guest => (
                format!(
                    "{what}:guest:{}",
                    ip.map_or_else(|| "unknown".into(), |ip| ip.to_string())
                ),
                settings.uploads.per_hour_guest,
            ),
            a if a.can_moderate() => return Ok(()),
            a => (
                format!("{what}:user:{}", a.user_id().map_or(0, |u| u.0)),
                settings.uploads.per_hour_user,
            ),
        };
        let per_hour = per_hour.max(1);
        self.limiter.check_rate(
            &key,
            std::time::Instant::now(),
            per_hour,
            std::time::Duration::from_secs(3600) / per_hour,
        )
    }

    /// What URL uploads may reach. Public addresses only, unless a test
    /// suite's mock server on loopback needs reaching.
    pub fn with_url_reach(mut self, reach: crate::fetch::Reach) -> Self {
        self.fetcher = crate::fetch::Fetcher::new(reach);
        self
    }

    /// Where uploads are buffered. An API layer that must hold a request
    /// body before it can authenticate (a multipart body whose key field
    /// comes after its file) buffers here too, on the same filesystem.
    pub(crate) fn media(&self) -> &Arc<dyn MediaProcessor> {
        &self.media
    }

    /// Where the operator's watermark image lives: beside the temp
    /// directory, under the data directory.
    pub fn watermark_path(&self) -> PathBuf {
        self.temp_dir
            .parent()
            .unwrap_or(&self.temp_dir)
            .join("branding")
            .join("watermark")
    }

    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    /// Public URLs for a stored file.
    pub fn urls(&self, file: &MediaFile, delete_token: Option<&str>) -> FileUrls {
        let base = self.settings.get().site.base_url.clone();
        let storage = file.storage_id.and_then(|id| self.storages.get(id));
        let derivatives = match (storage, object_keys(file)) {
            (Some(storage), Ok(found)) => found
                .into_iter()
                .map(|(d, key)| (d.legacy_key(), storage.url_for(&key)))
                .collect(),
            _ => BTreeMap::new(),
        };
        FileUrls {
            viewer: format!("{base}/f/{}", file.id_encoded),
            delete: delete_token.map(|t| format!("{base}/delete/{}/{t}", file.id_encoded)),
            derivatives,
        }
    }

    pub async fn upload(&self, cmd: UploadCommand) -> Result<UploadedFile, DomainError> {
        use crate::metrics::UploadOutcome;
        let result = match self.check_rate("upload", cmd.actor, cmd.uploader_ip) {
            Ok(()) => self.run(cmd).await,
            Err(error) => Err(error),
        };
        match &result {
            Ok(uploaded) if uploaded.duplicate => self.metrics.upload(UploadOutcome::Duplicate, 0),
            Ok(uploaded) => self
                .metrics
                .upload(UploadOutcome::Stored, uploaded.file.size),
            Err(error) if refused(error) => self.metrics.upload(UploadOutcome::Refused, 0),
            Err(_) => self.metrics.upload(UploadOutcome::Failed, 0),
        }
        result
    }

    async fn run(&self, cmd: UploadCommand) -> Result<UploadedFile, DomainError> {
        let settings = self.settings.get();
        let now = Utc::now();
        let actor = cmd.actor;

        // 1. Authorise.
        if !settings.may_upload(actor) {
            return Err(DomainError::Forbidden);
        }
        let album = self.resolve_album(actor, cmd.album.as_deref()).await?;
        if let Some(category) = cmd.category
            && categories::find_by_id(&self.db, category)
                .await
                .map_err(database)?
                .is_none()
        {
            return Err(DomainError::invalid_param(
                "category_id",
                "no such category",
            ));
        }

        // 2. Ingest the bytes to a temporary file, within the actor's limit.
        let mut temps = Temps::default();
        let limit = settings.max_upload_size(actor);
        let source = match cmd.source {
            // Off unless an administrator turned it on: a URL upload makes
            // the server send a request on a stranger's behalf.
            UploadSource::Url(_) if !settings.uploads.allow_url_sources => {
                return Err(DomainError::UrlFetchRejected(
                    "URL uploads are not available on this instance".into(),
                ));
            }
            UploadSource::Url(url) => {
                let fetched = self.fetcher.fetch(&url, limit).await?;
                UploadSource::Stream {
                    reader: fetched.reader,
                    filename: fetched.filename,
                }
            }
            other => other,
        };
        let ingested = self.ingest(source, limit, &mut temps).await?;

        // 3. Sniff and probe. The real type comes from the bytes alone.
        let probe = self.media.probe(&ingested.path).await?;
        if !settings.extension_allowed(&probe.extension) {
            return Err(DomainError::ExtensionNotAllowed(probe.extension));
        }
        // Audio is Pro. Checked on the bytes' real kind, so renaming a
        // file does not get round it.
        if probe.kind == MediaKind::Audio && !self.entitlements().pro {
            return Err(DomainError::ProRequired("audio"));
        }
        // The decompression-bomb guard: probing never decodes, so this check
        // comes before anything that does.
        if probe.pixels() > settings.uploads.max_pixels {
            return Err(DomainError::PixelLimit {
                pixels: probe.pixels(),
                limit: settings.uploads.max_pixels,
            });
        }

        // 4. Stop words, in everything the uploader wrote.
        let stop_words = &settings.uploads.stop_words;
        if !stop_words.is_empty() {
            let mut texts: Vec<&str> = cmd.tags.iter().map(String::as_str).collect();
            texts.extend(cmd.title.as_deref());
            texts.extend(cmd.description.as_deref());
            texts.extend(ingested.filename.as_deref());
            if contains_stop_word(stop_words, &texts) {
                return Err(DomainError::StopWord);
            }
        }

        // 5. Duplicates: the same bytes from the same owner (or from any
        // guest) return the existing upload — kind to retry loops.
        if settings.uploads.detect_duplicates
            && let Some(existing) =
                files::find_duplicate(&self.db, &ingested.source_md5, actor.user_id())
                    .await
                    .map_err(database)?
        {
            return self.existing(existing, actor).await;
        }

        // EXIF is read from the upload as sent, before anything strips it.
        let exif = self.media.read_exif(&ingested.path).await?;

        // 6-7. Transform: a `width` resize keeps the untouched original.
        let mut served = ingested.path.clone();
        let mut original = None;
        if let Some(width) = cmd.width {
            let resized = self.media.resize(&ingested.path, width).await?;
            if resized != ingested.path {
                served = temps.track(resized);
                original = Some((ingested.path.clone(), probe.clone()));
            }
        }
        // The operator's mark, for the roles it applies to, when one has
        // been set. The untouched upload is kept as the original, as for a
        // resize.
        let mark = self.watermark_path();
        if settings.watermark.applies_to(actor)
            && tokio::fs::try_exists(&mark).await.unwrap_or(false)
        {
            let w = &settings.watermark;
            let marked = self
                .media
                .watermark(
                    &served,
                    &Watermark {
                        mark,
                        position: w.position,
                        width_percent: w.percentage,
                        opacity_percent: w.opacity,
                        margin: w.margin,
                        min_width: w.min_width,
                        min_height: w.min_height,
                    },
                )
                .await?;
            if marked != served {
                served = temps.track(marked);
                if original.is_none() {
                    original = Some((ingested.path.clone(), probe.clone()));
                }
            }
        }
        let mut served_probe = if original.is_some() {
            self.media.probe(&served).await?
        } else {
            probe.clone()
        };
        if settings.derivatives.strip_exif {
            self.media.strip_metadata(&served, &served_probe).await?;
            // A retained original would otherwise carry everything the strip
            // removed.
            if let Some((path, original_probe)) = &original {
                self.media.strip_metadata(path, original_probe).await?;
            }
            served_probe = self.media.probe(&served).await?;
        }
        let checksum = md5_file(&served).await?;

        // 8. Derive thumb, medium and frame from the served file.
        let plan = DerivativePlan::new(served_probe.kind, &settings.derivatives);
        let mut derived = self.media.derive(&served, &served_probe, &plan).await?;
        for file in derived.files.values() {
            if file.path != served {
                temps.track(file.path.clone());
            }
        }
        if let Some((path, original_probe)) = &original {
            derived.insert(
                Derivative::Original,
                DerivativeFile {
                    path: path.clone(),
                    size: tokio::fs::metadata(path).await.map_err(io)?.len(),
                    width: original_probe.width,
                    height: original_probe.height,
                    mime: original_probe.mime.clone(),
                    extension: original_probe.extension.clone(),
                },
            );
        }
        let chain = derived.chain();

        // 9. Choose a storage with room for every object.
        let total: u64 = derived.files.values().map(|f| f.size).sum();
        let record = storages::pick_for_upload(&self.db, total)
            .await
            .map_err(database)?
            .ok_or(DomainError::StorageFull)?;
        let storage = self
            .storages
            .get(record.id)
            .ok_or(DomainError::StorageFull)?;
        let layout = self
            .storages
            .layout(record.id)
            .ok_or(DomainError::StorageFull)?
            .clone();

        // 10. Insert the row, then upload the bytes — in that order: the key
        // needs the public id, and the only failure left is a pending row,
        // which nothing lists and which is removed here or by the sweep.
        let name = keys::stored_name(
            ingested.filename.as_deref().unwrap_or("file"),
            &random_base62(8),
        );
        let image = derived
            .get(Derivative::Image)
            .ok_or_else(|| DomainError::Internal("no served file".into()))?
            .clone();
        let delete_token = hex::encode(rand::rng().random::<[u8; 16]>());
        let needs_moderation = settings.needs_moderation(actor);
        let expires_at = cmd
            .expiration
            .map(|d| d.min(settings.uploads.max_expiration))
            .and_then(|d| ChronoDuration::from_std(d).ok())
            .map(|d| now + d);
        let size_of = |d| derived.get(d).map(|f| f.size);

        // The key closure is called through `Fn`; a mutex (not a RefCell,
        // which would make this future !Send) carries any error out of it.
        let key_error = std::sync::Mutex::new(None);
        let pending = files::create_pending(
            &self.db,
            &files::NewFile {
                kind: served_probe.kind,
                name: &name,
                extension: &served_probe.extension,
                original_extension: original.as_ref().map(|(_, p)| p.extension.as_str()),
                mime: &served_probe.mime,
                original_filename: ingested.filename.as_deref(),
                title: cmd.title.as_deref(),
                description: cmd.description.as_deref(),
                size: image.size,
                width: served_probe.width,
                height: served_probe.height,
                duration_ms: served_probe.duration_ms,
                is_animated: served_probe.is_animated,
                frame_count: served_probe.frame_count,
                chain,
                thumb_size: size_of(Derivative::Thumb),
                medium_size: size_of(Derivative::Medium),
                frame_size: size_of(Derivative::Frame),
                checksum: &checksum,
                source_checksum: Some(&ingested.source_md5),
                sha256: Some(&ingested.sha256),
                storage_id: Some(record.id),
                storage_mode: record.path_mode,
                user_id: actor.user_id(),
                album_id: album.as_ref().map(|a| a.id),
                category_id: cmd.category,
                nsfw: cmd.nsfw.unwrap_or(settings.content.default_nsfw),
                is_approved: !needs_moderation,
                is_360: false,
                exif: exif.as_deref(),
                uploader_ip: cmd.uploader_ip,
                delete_token: Some(&delete_token),
                expires_at,
            },
            |id_encoded| {
                let parts = KeyParts {
                    created_at: now,
                    user_id: actor.user_id(),
                    id_encoded,
                    name: &name,
                    extension: &served_probe.extension,
                };
                keys::object_key(&layout, &parts, Derivative::Image).unwrap_or_else(|e| {
                    if let Ok(mut slot) = key_error.lock() {
                        *slot = Some(e);
                    }
                    String::new()
                })
            },
        )
        .await;
        if let Some(e) = key_error.into_inner().ok().flatten() {
            if let Ok(row) = &pending {
                let _ = files::purge(&self.db, row.id).await;
            }
            return Err(e.into());
        }
        let pending = pending?;

        let file_keys = object_keys(&pending)?;
        let mut stored: Vec<String> = Vec::new();
        for (derivative, key) in &file_keys {
            let Some(file) = derived.get(*derivative) else {
                continue;
            };
            let put = storage
                .put(key, PutBody::Path(file.path.clone()), &file.mime)
                .await;
            if let Err(error) = put {
                for key in &stored {
                    let _ = storage.delete(key).await;
                }
                let _ = files::purge(&self.db, pending.id).await;
                return Err(error.into());
            }
            stored.push(key.clone());
        }
        files::mark_ready(&self.db, pending.id)
            .await
            .map_err(database)?;

        // 11. Side effects.
        if !cmd.tags.is_empty() {
            let names: Vec<&str> = cmd.tags.iter().map(String::as_str).collect();
            tags::set_for_file(&self.db, pending.id, &names)
                .await
                .map_err(database)?;
        }
        if needs_moderation {
            moderation::enqueue(&self.db, pending.id, None)
                .await
                .map_err(database)?;
            // Held files are not served until a moderator approves them, so
            // their bytes wait in quarantine like a deleted file's. The
            // legacy response still names `frame.url` — that is its shape —
            // but nothing answers at it until approval releases the bytes.
            for key in &stored {
                storage.quarantine(key).await?;
            }
        }

        // 12. The rich result.
        self.result(pending.id, Some(&delete_token), false).await
    }

    /// A file as `actor` may see it, by public id: live, and approved unless
    /// the actor can manage it, and not in a private or password album
    /// unless the actor can manage that. Anything else is `NotFound` —
    /// whether a hidden file exists is not the caller's business.
    pub async fn view(
        &self,
        actor: Actor,
        id_encoded: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<UploadedFile, DomainError> {
        self.view_for(actor, id_encoded, now, &[]).await
    }

    /// [`Uploads::view`], with the password albums this caller unlocked.
    pub async fn view_for(
        &self,
        actor: Actor,
        id_encoded: &str,
        now: chrono::DateTime<Utc>,
        unlocked: &[haloid_core::ids::AlbumId],
    ) -> Result<UploadedFile, DomainError> {
        let file = files::find_by_id_encoded(&self.db, id_encoded)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("file"))?;
        let manages = actor.can_manage(file.user_id);
        if !file.is_live(now) || !(file.is_approved || manages) {
            return Err(DomainError::NotFound("file"));
        }
        if let Some(album) = file.album_id {
            let album = albums::find_by_id(&self.db, album)
                .await
                .map_err(database)?;
            let hidden = album.is_some_and(|a| {
                let closed = match a.privacy {
                    haloid_core::files::AlbumPrivacy::Private => true,
                    haloid_core::files::AlbumPrivacy::Password => !unlocked.contains(&a.id),
                    _ => false,
                };
                closed && !actor.can_manage(a.user_id)
            });
            if hidden {
                return Err(DomainError::NotFound("file"));
            }
        }
        self.result(file.id, None, false).await
    }

    /// Count a view of a file's page. Whoever may manage the file is not
    /// counted, and one viewer (an account, else an address) counts once per
    /// file per half hour, so a reload or a back-and-forth is not a view.
    /// Views of a file someone may not see answer as if it did not exist.
    pub async fn record_view(
        &self,
        actor: Actor,
        ip: Option<std::net::IpAddr>,
        id_encoded: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<(), DomainError> {
        let file = files::find_by_id_encoded(&self.db, id_encoded)
            .await
            .map_err(database)?
            .filter(|f| f.is_live(now) && (f.is_approved || actor.can_manage(f.user_id)))
            .ok_or(DomainError::NotFound("file"))?;
        if actor.can_manage(file.user_id) {
            return Ok(());
        }
        let who = match (actor.user_id(), ip) {
            (Some(user), _) => format!("u{}", user.0),
            (None, Some(ip)) => format!("a{ip}"),
            (None, None) => "unknown".into(),
        };
        let fresh = {
            let mut viewed = self.viewed.lock().unwrap_or_else(|e| e.into_inner());
            let at = std::time::Instant::now();
            if viewed.len() > VIEWS_REMEMBERED {
                viewed.retain(|_, seen| at.duration_since(*seen) < VIEW_WINDOW);
            }
            match viewed.get(&(file.id.0, who.clone())) {
                Some(seen) if at.duration_since(*seen) < VIEW_WINDOW => false,
                _ => {
                    viewed.insert((file.id.0, who), at);
                    true
                }
            }
        };
        if fresh {
            files::increment_views(&self.db, file.id)
                .await
                .map_err(database)?;
        }
        Ok(())
    }

    pub(crate) async fn resolve_album(
        &self,
        actor: Actor,
        album: Option<&str>,
    ) -> Result<Option<Album>, DomainError> {
        let Some(id) = album.filter(|a| !a.is_empty()) else {
            return Ok(None);
        };
        let album = albums::find_by_id_encoded(&self.db, id)
            .await
            .map_err(database)?
            .ok_or_else(|| DomainError::invalid_param("album_id", "no such album"))?;
        if !actor.can_manage(album.user_id) {
            return Err(DomainError::invalid_param("album_id", "not your album"));
        }
        Ok(Some(album))
    }

    /// An earlier upload returned as a duplicate. Only its owner gets its
    /// delete link back: a guest re-uploading bytes some other guest sent
    /// must not be handed a way to delete theirs.
    async fn existing(&self, file: MediaFile, actor: Actor) -> Result<UploadedFile, DomainError> {
        let token = if actor.owns(file.user_id) {
            files::delete_token(&self.db, file.id)
                .await
                .map_err(database)?
        } else {
            None
        };
        self.result(file.id, token.as_deref(), true).await
    }

    pub(crate) async fn result(
        &self,
        id: FileId,
        delete_token: Option<&str>,
        duplicate: bool,
    ) -> Result<UploadedFile, DomainError> {
        let file = files::find_by_id(&self.db, id)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("file"))?;
        let tags = tags::for_file(&self.db, id).await.map_err(database)?;
        let album = match file.album_id {
            Some(album) => albums::find_by_id(&self.db, album)
                .await
                .map_err(database)?,
            None => None,
        };
        let owner = match file.user_id {
            Some(user) => users::find_by_id(&self.db, user)
                .await
                .map_err(database)?
                .map(|u| u.username),
            None => None,
        };
        let urls = self.urls(&file, delete_token);
        let exif = files::exif(&self.db, id).await.map_err(database)?;
        let storage_kind = file
            .storage_id
            .and_then(|id| self.storages.get(id))
            .map(|s| s.kind());
        let delete_hash = match delete_token {
            Some(token) => {
                let token = token.to_string();
                Some(
                    tokio::task::spawn_blocking(move || {
                        crate::auth::secrets::hash_password(&token)
                    })
                    .await
                    .map_err(|e| DomainError::Internal(e.to_string().into()))??,
                )
            }
            None => None,
        };
        Ok(UploadedFile {
            file,
            tags,
            album,
            owner,
            urls,
            duplicate,
            exif,
            storage_kind,
            delete_hash,
        })
    }

    async fn ingest(
        &self,
        source: UploadSource,
        limit: u64,
        temps: &mut Temps,
    ) -> Result<Ingested, DomainError> {
        tokio::fs::create_dir_all(&self.temp_dir)
            .await
            .map_err(io)?;
        let path = temps.track(self.temp_dir.join(format!("upload-{}", random_base62(16))));
        let mut md5 = Md5::new();
        let mut sha = Sha256::new();

        let filename = match source {
            // Fetched into a stream before this point.
            UploadSource::Url(_) => {
                return Err(DomainError::UrlFetchRejected("not fetched".into()));
            }
            UploadSource::Stream {
                mut reader,
                filename,
            } => {
                let mut out = tokio::fs::File::create(&path).await.map_err(io)?;
                let mut buf = vec![0u8; 64 * 1024];
                let mut total: u64 = 0;
                loop {
                    let n = reader.read(&mut buf).await.map_err(io)?;
                    if n == 0 {
                        break;
                    }
                    total += n as u64;
                    // Refused the moment the limit is passed, not after the
                    // whole body has landed on disk.
                    if total > limit {
                        return Err(DomainError::SizeLimit { size: total, limit });
                    }
                    md5.update(&buf[..n]);
                    sha.update(&buf[..n]);
                    out.write_all(&buf[..n]).await.map_err(io)?;
                }
                out.flush().await.map_err(io)?;
                if total == 0 {
                    return Err(DomainError::EmptySource);
                }
                filename
            }
            UploadSource::Base64 { data, filename } => {
                let bytes = decode_base64(&data, limit)?;
                md5.update(&bytes);
                sha.update(&bytes);
                tokio::fs::write(&path, &bytes).await.map_err(io)?;
                filename
            }
        };
        Ok(Ingested {
            path,
            filename,
            source_md5: hex::encode(md5.finalize()),
            sha256: hex::encode(sha.finalize()),
        })
    }
}

/// Decode a base64 source: an optional `data:…;base64,` prefix, whitespace
/// anywhere (curl and ShareX both introduce it), standard or URL-safe
/// alphabet, padding optional.
fn decode_base64(data: &str, limit: u64) -> Result<Vec<u8>, DomainError> {
    let mut body = data.trim();
    if body.starts_with("data:") {
        body = body
            .split_once(";base64,")
            .map(|(_, rest)| rest)
            .ok_or(DomainError::InvalidBase64)?;
    }
    let compact: String = body.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if compact.is_empty() {
        return Err(DomainError::EmptySource);
    }
    // Refuse an oversized source before decoding it.
    let decoded_len = (compact.len() as u64 / 4) * 3;
    if decoded_len > limit.saturating_add(3) {
        return Err(DomainError::SizeLimit {
            size: decoded_len,
            limit,
        });
    }
    let engines = [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    let bytes = engines
        .iter()
        .find_map(|engine| engine.decode(&compact).ok())
        .ok_or(DomainError::InvalidBase64)?;
    if bytes.len() as u64 > limit {
        return Err(DomainError::SizeLimit {
            size: bytes.len() as u64,
            limit,
        });
    }
    if bytes.is_empty() {
        return Err(DomainError::EmptySource);
    }
    Ok(bytes)
}

async fn md5_file(path: &Path) -> Result<String, DomainError> {
    let mut file = tokio::fs::File::open(path).await.map_err(io)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await.map_err(io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Whether an upload failed for something about the request rather than
/// for a fault of ours: a file the tools cannot read is the uploader's.
fn refused(error: &DomainError) -> bool {
    use haloid_core::MediaError;
    error.is_client_safe()
        || matches!(
            error,
            DomainError::Media(MediaError::ProbeFailed(_) | MediaError::Unsupported(_))
        )
}

/// Whether any of `texts` contains one of the operator's stop words, ignoring
/// case. Uploads and later edits are held to the same list.
pub(crate) fn contains_stop_word(stop_words: &[String], texts: &[&str]) -> bool {
    texts.iter().any(|text| {
        let text = text.to_lowercase();
        stop_words.iter().any(|w| text.contains(&w.to_lowercase()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time: the pipeline must be spawnable on a multi-threaded
    /// runtime. A `RefCell` held across an await once made it `!Send`, and
    /// only the first HTTP handler to call it noticed.
    #[allow(dead_code)]
    fn upload_is_send(uploads: &Uploads, cmd: UploadCommand) {
        fn send<T: Send>(_: T) {}
        send(uploads.upload(cmd));
    }

    #[test]
    fn base64_accepts_what_clients_actually_send() {
        let limit = 1024;
        for data in [
            "aGVsbG8=",
            "aGVsbG8",
            "data:image/png;base64,aGVsbG8=",
            "  aGVs\nbG8=\r\n",
            "aGVs bG8=",
        ] {
            assert_eq!(decode_base64(data, limit).unwrap(), b"hello", "{data:?}");
        }
    }

    #[test]
    fn bad_base64_is_its_own_error() {
        for data in ["!!!!", "data:image/png,aGVsbG8="] {
            assert!(
                matches!(decode_base64(data, 1024), Err(DomainError::InvalidBase64)),
                "{data:?}"
            );
        }
        assert!(matches!(
            decode_base64("   ", 1024),
            Err(DomainError::EmptySource)
        ));
    }

    #[test]
    fn oversized_base64_is_refused_before_decoding() {
        let big = "A".repeat(4000);
        assert!(matches!(
            decode_base64(&big, 1000),
            Err(DomainError::SizeLimit { limit: 1000, .. })
        ));
    }
}
