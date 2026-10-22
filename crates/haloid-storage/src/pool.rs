//! The configured storages, ready to use.
//!
//! Built from the `ih_storages` rows (which the service loads) and the data
//! directory. Every storage is built, retired ones included: a retired
//! storage takes no uploads but still serves what it holds. Choosing where a
//! new upload goes is the database's job (`haloid_db::storages::
//! pick_for_upload`), since it depends on live usage counters.
//!
//! A configuration that cannot work fails the whole load and names the
//! storage, so a typo surfaces at startup rather than on the first upload.
//! A storage of a kind this build has no backend for is the exception: it
//! is left out and named in [`StoragePool::unavailable`], so a Community
//! build started on a Pro database still runs.
//!
//! The pool can be rebuilt while the server runs ([`StoragePool::reload`]),
//! so a storage added or changed in the admin area is used at once. Readers
//! always see one whole set: the new one is built aside and swapped in.

use crate::backend::{EnvLookup, StorageBackend};
use crate::keys::{Layout, Template, validate_key};
use crate::local::LocalStorage;
use arc_swap::ArcSwap;
use haloid_core::StorageError;
use haloid_core::ids::StorageId;
use haloid_core::storage::{Storage, StorageKind, StoragePathMode, StorageRecord};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A local storage's `config` JSON. Unknown fields are refused, so a
/// misspelt `base_path` is an error instead of a silently ignored setting.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalConfig {
    /// The served directory. Default `<data dir>/storage/<name>`.
    base_path: Option<PathBuf>,
    /// The never-served directory for deleted files. Default
    /// `<data dir>/quarantine/<name>`.
    quarantine_path: Option<PathBuf>,
}

pub struct StoragePool {
    current: ArcSwap<Snapshot>,
    data_dir: PathBuf,
    backends: Vec<Arc<dyn StorageBackend>>,
}

/// One built set of storages.
struct Snapshot {
    storages: HashMap<StorageId, Arc<dyn Storage>>,
    layouts: HashMap<StorageId, Layout>,
    served: Vec<ServedRoot>,
    kinds: Vec<StorageKind>,
    unavailable: Vec<String>,
}

/// A local storage's public directory and the URL prefix it is served
/// under, for the server to mount. Quarantine directories are never here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedRoot {
    pub url_prefix: String,
    pub root: PathBuf,
}

/// A storage that cannot be built as configured, named.
pub fn misconfigured(record: &StorageRecord, reason: impl std::fmt::Display) -> StorageError {
    StorageError::Misconfigured(format!("storage {:?}: {reason}", record.name))
}

impl StoragePool {
    /// Relative paths in a storage's configuration are taken relative to
    /// `data_dir`. Kinds other than local need a backend in `backends`;
    /// their credentials are read from the process environment.
    pub fn from_records(
        records: &[StorageRecord],
        data_dir: &Path,
        backends: &[Arc<dyn StorageBackend>],
    ) -> Result<Self, StorageError> {
        Self::from_records_with_env(records, data_dir, backends, &|name| {
            std::env::var(name).ok()
        })
    }

    /// The same, with the credentials looked up through `env`.
    pub fn from_records_with_env(
        records: &[StorageRecord],
        data_dir: &Path,
        backends: &[Arc<dyn StorageBackend>],
        env: EnvLookup<'_>,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            current: ArcSwap::from_pointee(build(records, data_dir, backends, env)?),
            data_dir: data_dir.to_path_buf(),
            backends: backends.to_vec(),
        })
    }

    /// Rebuild from the current rows, credentials from the process
    /// environment. On an error the pool in use is left as it was.
    pub fn reload(&self, records: &[StorageRecord]) -> Result<(), StorageError> {
        self.reload_with_env(records, &|name| std::env::var(name).ok())
    }

    pub fn reload_with_env(
        &self,
        records: &[StorageRecord],
        env: EnvLookup<'_>,
    ) -> Result<(), StorageError> {
        let next = build(records, &self.data_dir, &self.backends, env)?;
        self.current.store(Arc::new(next));
        Ok(())
    }

    /// Whether one storage would build as configured, credentials included,
    /// without touching the pool: for checking an admin's change first.
    pub fn check(&self, record: &StorageRecord) -> Result<(), StorageError> {
        build(
            std::slice::from_ref(record),
            &self.data_dir,
            &self.backends,
            &|name| std::env::var(name).ok(),
        )
        .map(|_| ())
    }

    pub fn get(&self, id: StorageId) -> Option<Arc<dyn Storage>> {
        self.current.load().storages.get(&id).cloned()
    }

    /// Every local storage's served directory, as the pool is now.
    pub fn served_roots(&self) -> Vec<ServedRoot> {
        self.current.load().served.clone()
    }

    /// How keys are laid out on this storage, for [`crate::keys::object_key`].
    pub fn layout(&self, id: StorageId) -> Option<Layout> {
        self.current.load().layouts.get(&id).cloned()
    }

    /// The kinds of storage this build can use.
    pub fn kinds(&self) -> Vec<StorageKind> {
        self.current.load().kinds.clone()
    }

    /// Storages left out because this build has no backend for their kind.
    pub fn unavailable(&self) -> Vec<String> {
        self.current.load().unavailable.clone()
    }
}

fn build(
    records: &[StorageRecord],
    data_dir: &Path,
    backends: &[Arc<dyn StorageBackend>],
    env: EnvLookup<'_>,
) -> Result<Snapshot, StorageError> {
    let mut kinds = vec![StorageKind::Local];
    kinds.extend(backends.iter().map(|b| b.kind()));
    let mut pool = Snapshot {
        storages: HashMap::new(),
        layouts: HashMap::new(),
        served: Vec::new(),
        kinds,
        unavailable: Vec::new(),
    };
    for record in records {
        let layout = layout(record)?;
        let storage: Arc<dyn Storage> = if record.kind == StorageKind::Local {
            let storage = local(record, data_dir)?;
            pool.served.push(ServedRoot {
                url_prefix: record.url_prefix.clone(),
                root: storage.root().to_path_buf(),
            });
            Arc::new(storage)
        } else if let Some(backend) = backends.iter().find(|b| b.kind() == record.kind) {
            backend.build(record, env)?
        } else {
            tracing::warn!(
                storage = %record.name,
                kind = record.kind.as_str(),
                "this build cannot use this kind of storage (it is a Haloid Pro \
                 feature); its files will not be served"
            );
            pool.unavailable.push(record.name.clone());
            continue;
        };
        pool.storages.insert(record.id, storage);
        pool.layouts.insert(record.id, layout);
    }
    Ok(pool)
}

fn layout(record: &StorageRecord) -> Result<Layout, StorageError> {
    Ok(match record.path_mode {
        StoragePathMode::DateFolder => Layout::DateFolder,
        StoragePathMode::Direct => Layout::Direct,
        StoragePathMode::Template => {
            let template = record
                .path_template
                .as_deref()
                .ok_or_else(|| misconfigured(record, "path mode needs a path template"))?;
            Layout::Template(Template::parse(template).map_err(|e| misconfigured(record, e))?)
        }
    })
}

fn local(record: &StorageRecord, data_dir: &Path) -> Result<LocalStorage, StorageError> {
    let config: LocalConfig =
        serde_json::from_str(&record.config).map_err(|e| misconfigured(record, e))?;

    // The name becomes a directory in the defaults below.
    if record.name.contains('/') || validate_key(&record.name).is_err() {
        return Err(misconfigured(
            record,
            "name is not usable as a directory name",
        ));
    }
    let resolve = |configured: Option<PathBuf>, default: &str| match configured {
        Some(path) if path.is_absolute() => path,
        Some(path) => data_dir.join(path),
        None => data_dir.join(default).join(&record.name),
    };
    let root = resolve(config.base_path, "storage");
    let quarantine = resolve(config.quarantine_path, "quarantine");

    LocalStorage::new(record.id, &record.url_prefix, &root, &quarantine)
        .map_err(|e| misconfigured(record, e))
}
