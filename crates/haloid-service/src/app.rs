//! The assembled service: everything a request handler can reach.
//!
//! The server builds one `App` at startup and hands both API surfaces an
//! `Arc` of it. Tests build theirs the same way, so the wiring under test is
//! the wiring that ships.

use crate::admin::Admin;
use crate::album_locks::AlbumLocks;
use crate::auth::{Auth, AuthConfig};
use crate::chunked::ChunkedUploads;
use crate::edit::Editor;
use crate::files::Files;
use crate::library::Library;
use crate::licensing::Licensing;
use crate::settings::SettingsHandle;
use crate::storages::StorageAdmin;
use crate::upload::Uploads;
use haloid_core::DomainError;
use haloid_core::processor::MediaProcessor;
use haloid_core::storage::{StorageKind, StoragePathMode};
use haloid_db::{Db, storages};
use haloid_storage::backend::StorageBackend;
use haloid_storage::pool::StoragePool;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub struct AppConfig {
    /// Where local storage, quarantine and temporary uploads live by default.
    pub data_dir: PathBuf,
    /// Signs ALTCHA challenges and password-album grants. Env only, never
    /// the database.
    pub altcha_secret: String,
    /// Settings pinned by the environment.
    pub pinned_settings: BTreeMap<String, String>,
    /// Proof-of-work parameters, when not the production ones — tests use a
    /// cheap challenge so a debug build can solve it in milliseconds.
    pub altcha: Option<crate::auth::human::AltchaConfig>,
    pub pro: ProConfig,
    /// The updater, as the server binary describes itself. `None` in tests
    /// that do not exercise it: the admin page then reports no updater.
    pub updates: Option<crate::updates::UpdatesConfig>,
}

/// What the Pro build adds, and the licence.
#[derive(Clone)]
pub struct ProConfig {
    /// Kinds of storage beyond local. Empty in the Community build.
    pub storage_backends: Vec<Arc<dyn StorageBackend>>,
    /// `HALOID_LICENCE_KEY`, which wins over a key installed from the
    /// admin area.
    pub licence_key: Option<String>,
    /// The public keys a licence must be signed with: the official ones,
    /// or a test's own.
    pub trusted_keys: Vec<[u8; 32]>,
}

impl Default for ProConfig {
    fn default() -> Self {
        Self {
            storage_backends: Vec::new(),
            licence_key: None,
            trusted_keys: haloid_licence::OFFICIAL_KEYS.to_vec(),
        }
    }
}

pub struct App {
    pub db: Db,
    pub settings: SettingsHandle,
    pub auth: Auth,
    pub uploads: Arc<Uploads>,
    pub chunked: ChunkedUploads,
    pub album_locks: AlbumLocks,
    pub files: Files,
    pub library: Library,
    pub editor: Editor,
    pub storage_admin: StorageAdmin,
    pub admin: Admin,
    pub metrics: Arc<crate::metrics::Metrics>,
    pub storages: Arc<StoragePool>,
    pub licensing: Arc<Licensing>,
    pub updates: Arc<crate::updates::Updates>,
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

impl App {
    /// [`App::start`] with the production media toolkit — `vips`, `ffmpeg`
    /// and `ffprobe` from the PATH. The API crates' tests use this, since
    /// they may not depend on `haloid-media` themselves.
    pub async fn start_default(db: Db, config: AppConfig) -> Result<Self, DomainError> {
        let media = haloid_media::MediaToolkit::new(haloid_media::MediaConfig::default());
        Self::start(db, Arc::new(media), config).await
    }

    /// Assemble the service over a migrated database.
    ///
    /// A fresh install has no storage configured, and every upload would be
    /// `StorageFull`; so if there is none, a local one called `main` is
    /// created under the data directory, served at `<base_url>/files`.
    pub async fn start(
        db: Db,
        media: Arc<dyn MediaProcessor>,
        config: AppConfig,
    ) -> Result<Self, DomainError> {
        let settings = SettingsHandle::load(db.clone(), config.pinned_settings).await?;

        let mut records = storages::list(&db).await.map_err(database)?;
        if records.is_empty() {
            let base_url = settings.get().site.base_url.clone();
            let created = storages::create(
                &db,
                &storages::StorageDetails {
                    name: "main".into(),
                    kind: StorageKind::Local,
                    config: "{}".into(),
                    url_prefix: format!("{base_url}/files"),
                    path_mode: StoragePathMode::DateFolder,
                    path_template: None,
                    capacity_bytes: None,
                    is_active: true,
                    priority: 0,
                },
            )
            .await?;
            tracing::info!(
                url_prefix = %created.url_prefix,
                "no storage configured; created local storage \"main\""
            );
            records.push(created);
        }
        let pool = Arc::new(StoragePool::from_records(
            &records,
            &config.data_dir,
            &config.pro.storage_backends,
        )?);
        let licensing = Arc::new(
            Licensing::load(db.clone(), config.pro.trusted_keys, config.pro.licence_key).await?,
        );

        let album_locks = AlbumLocks::new(db.clone(), &config.altcha_secret);
        let mut auth_config = AuthConfig::new(config.altcha_secret);
        if let Some(altcha) = config.altcha {
            auth_config.altcha = altcha;
        }
        let auth = Auth::new(db.clone(), settings.clone(), auth_config);
        // Under the data directory, beside local storage, so finished
        // uploads are renamed into place rather than copied.
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let uploads = Arc::new(
            Uploads::new(
                db.clone(),
                settings.clone(),
                media,
                pool.clone(),
                config.data_dir.join("tmp"),
            )
            .with_metrics(metrics.clone())
            .with_licensing(licensing.clone()),
        );
        let files = Files::new(db.clone(), pool.clone());
        let library = Library::new(db.clone(), settings.clone(), uploads.clone());
        let admin = Admin::new(
            db.clone(),
            settings.clone(),
            uploads.clone(),
            Files::new(db.clone(), pool.clone()),
        )
        .with_pro(licensing.clone(), config.pro.storage_backends.clone())
        .with_storage_pool(pool.clone());
        let chunked = ChunkedUploads::new(db.clone(), settings.clone(), uploads.clone());
        let editor = Editor::new(
            db.clone(),
            settings.clone(),
            uploads.clone(),
            Files::new(db.clone(), pool.clone()),
        );

        let updates = crate::updates::Updates::new(
            db.clone(),
            settings.clone(),
            Some(licensing.clone()),
            config
                .updates
                .unwrap_or_else(crate::updates::UpdatesConfig::none),
        );
        Ok(Self {
            db,
            settings,
            auth,
            uploads,
            chunked,
            album_locks,
            files,
            library,
            editor,
            storage_admin: StorageAdmin::new(pool.clone()),
            admin,
            metrics,
            storages: pool,
            licensing,
            updates,
        })
    }
}

impl App {
    /// The metrics in Prometheus's text format, with the gauges read now.
    pub async fn metrics_text(&self) -> Result<String, DomainError> {
        let moderation = haloid_db::moderation::pending_count(&self.db)
            .await
            .map_err(database)?;
        let pending = haloid_db::jobs::pending_count(&self.db)
            .await
            .map_err(database)?;
        let failed = haloid_db::jobs::failed_count(&self.db)
            .await
            .map_err(database)?;
        Ok(self.metrics.render(&[
            (
                "haloid_moderation_pending",
                "Files waiting for a moderator.",
                moderation,
            ),
            (
                "haloid_jobs_pending",
                "Background jobs waiting to run.",
                pending,
            ),
            (
                "haloid_jobs_failed",
                "Background jobs that ran out of attempts.",
                failed,
            ),
        ]))
    }
}
