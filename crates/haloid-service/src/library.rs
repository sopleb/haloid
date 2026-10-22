//! Reading the library: lists of files, albums and users, as a given actor
//! may see them.
//!
//! The visibility rules live here, in one place, so both API surfaces and
//! the server's link-preview tags agree: what a guest can list, what an owner
//! sees of their own things, which private album answers whom. Anything an
//! actor may not see is `NotFound`, never `Forbidden` — whether a hidden
//! thing exists is not their business.

use crate::settings::SettingsHandle;
use crate::upload::{FileUrls, Uploads};
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::account::User;
use haloid_core::auth::Actor;
use haloid_core::files::{Album, AlbumPrivacy, MediaFile};
use haloid_core::ids::AlbumId;
use haloid_core::media::MediaKind;
use haloid_core::pagination::{AlbumSort, Cursor, FileSort, Page, clamp_limit};
use haloid_db::files::{FileQuery, FileScope, Visibility};
use haloid_db::{Db, albums, files, users};
use std::sync::Arc;

/// A file in a list: the record and its URLs, without the extras a single
/// file's page carries.
#[derive(Debug, Clone)]
pub struct Listed {
    pub file: MediaFile,
    pub urls: FileUrls,
}

/// What a list request may ask for.
#[derive(Debug, Clone, Default)]
pub struct ListRequest {
    pub sort: Option<FileSort>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub kind: Option<MediaKind>,
}

/// What an old Chevereto link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyKind {
    File,
    Album,
}

pub struct Library {
    db: Db,
    settings: SettingsHandle,
    uploads: Arc<Uploads>,
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

impl Library {
    pub fn new(db: Db, settings: SettingsHandle, uploads: Arc<Uploads>) -> Self {
        Self {
            db,
            settings,
            uploads,
        }
    }

    /// Guests see NSFW only where an administrator allows it; members
    /// always may.
    fn include_nsfw(&self, actor: Actor) -> bool {
        !actor.is_guest() || self.settings.get().content.guests_see_nsfw
    }

    async fn list(
        &self,
        actor: Actor,
        scope: FileScope,
        visibility: Visibility,
        request: &ListRequest,
        default_sort: FileSort,
        now: DateTime<Utc>,
    ) -> Result<Page<Listed, FileSort>, DomainError> {
        let sort = request.sort.unwrap_or(default_sort);
        let after = request
            .cursor
            .as_deref()
            .map(|c| Cursor::parse(c, sort))
            .transpose()?;
        let page = files::list(
            &self.db,
            &FileQuery {
                scope,
                visibility,
                include_nsfw: self.include_nsfw(actor),
                kind: request.kind,
                sort,
                after,
                limit: clamp_limit(request.limit),
                now,
            },
        )
        .await?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(|file| Listed {
                    urls: self.uploads.urls(&file, None),
                    file,
                })
                .collect(),
            next: page.next,
        })
    }

    /// The public gallery: approved, live, in no hidden album, from no
    /// private profile.
    pub async fn explore(
        &self,
        actor: Actor,
        request: &ListRequest,
        now: DateTime<Utc>,
    ) -> Result<Page<Listed, FileSort>, DomainError> {
        if request.sort == Some(FileSort::AlbumOrder) {
            return Err(DomainError::invalid_param(
                "sort",
                "manual order exists only within an album",
            ));
        }
        self.list(
            actor,
            FileScope::Everyone,
            Visibility::Listed,
            request,
            FileSort::Newest,
            now,
        )
        .await
    }

    /// A user, by username, if the actor may see them: anyone for a public
    /// profile, only the user and moderators for a private one.
    pub async fn user(&self, actor: Actor, username: &str) -> Result<User, DomainError> {
        let user = users::find_by_username(&self.db, username)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("user"))?;
        if user.status == haloid_core::account::UserStatus::Banned && !actor.can_moderate() {
            return Err(DomainError::NotFound("user"));
        }
        if user.is_private && !actor.can_manage(Some(user.id)) {
            return Err(DomainError::NotFound("user"));
        }
        Ok(user)
    }

    /// A user's files: everything live to themselves and moderators, the
    /// public listing to anyone else.
    pub async fn user_files(
        &self,
        actor: Actor,
        username: &str,
        request: &ListRequest,
        now: DateTime<Utc>,
    ) -> Result<Page<Listed, FileSort>, DomainError> {
        let user = self.user(actor, username).await?;
        let visibility = if actor.can_manage(Some(user.id)) {
            Visibility::Owner
        } else {
            Visibility::Listed
        };
        self.list(
            actor,
            FileScope::User(user.id),
            visibility,
            request,
            FileSort::Newest,
            now,
        )
        .await
    }

    /// An album, if the actor may see it. Public and unlisted albums answer
    /// anyone who has the id; a password album whoever has unlocked it;
    /// private albums only those who can manage them.
    pub async fn album(&self, actor: Actor, id_encoded: &str) -> Result<Album, DomainError> {
        self.album_for(actor, id_encoded, &[]).await
    }

    /// [`Library::album`], with the password albums this caller has
    /// unlocked (see `album_locks`).
    pub async fn album_for(
        &self,
        actor: Actor,
        id_encoded: &str,
        unlocked: &[AlbumId],
    ) -> Result<Album, DomainError> {
        let album = albums::find_by_id_encoded(&self.db, id_encoded)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("album"))?;
        let open = match album.privacy {
            AlbumPrivacy::Public | AlbumPrivacy::Unlisted => true,
            AlbumPrivacy::Password => unlocked.contains(&album.id),
            AlbumPrivacy::Private => false,
        };
        if !open && album.privacy == AlbumPrivacy::Password && !actor.can_manage(album.user_id) {
            return Err(DomainError::AlbumLocked);
        }
        if !open && !actor.can_manage(album.user_id) {
            return Err(DomainError::NotFound("album"));
        }
        Ok(album)
    }

    /// An album's files, in its manual order by default.
    pub async fn album_files(
        &self,
        actor: Actor,
        id_encoded: &str,
        request: &ListRequest,
        now: DateTime<Utc>,
    ) -> Result<Page<Listed, FileSort>, DomainError> {
        self.album_files_for(actor, id_encoded, request, now, &[])
            .await
    }

    pub async fn album_files_for(
        &self,
        actor: Actor,
        id_encoded: &str,
        request: &ListRequest,
        now: DateTime<Utc>,
        unlocked: &[AlbumId],
    ) -> Result<Page<Listed, FileSort>, DomainError> {
        let album = self.album_for(actor, id_encoded, unlocked).await?;
        let visibility = if actor.can_manage(album.user_id) {
            Visibility::Owner
        } else {
            Visibility::Viewer
        };
        self.list(
            actor,
            FileScope::Album(album.id),
            visibility,
            request,
            FileSort::AlbumOrder,
            now,
        )
        .await
    }

    /// The public id of what an imported Chevereto link named: `image` or
    /// `album`, by its old public id. Visibility is left to the page the
    /// link is redirected to, which applies it as for any other visitor.
    pub async fn legacy_link(
        &self,
        what: LegacyKind,
        legacy_id: &str,
    ) -> Result<Option<String>, DomainError> {
        if legacy_id.is_empty()
            || legacy_id.len() > 32
            || !legacy_id.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Ok(None);
        }
        match what {
            LegacyKind::File => files::id_encoded_by_legacy(&self.db, legacy_id).await,
            LegacyKind::Album => albums::id_encoded_by_legacy(&self.db, legacy_id).await,
        }
        .map_err(database)
    }

    /// What to show on an album's card: its cover's URLs, as this actor may
    /// see it — the chosen cover, else the first picture or video in it.
    pub async fn cover(
        &self,
        actor: Actor,
        album: &Album,
        now: DateTime<Utc>,
    ) -> Result<Option<FileUrls>, DomainError> {
        let viewer = files::CoverViewer {
            manages: actor.can_manage(album.user_id),
            include_nsfw: self.include_nsfw(actor),
        };
        Ok(
            files::album_cover(&self.db, album.id, album.cover_file_id, viewer, now)
                .await
                .map_err(database)?
                .map(|file| self.uploads.urls(&file, None)),
        )
    }

    /// A user's top-level albums: all of them to themselves, the public ones
    /// to anyone else.
    pub async fn user_albums(
        &self,
        actor: Actor,
        username: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Page<Album, AlbumSort>, DomainError> {
        let user = self.user(actor, username).await?;
        let after = cursor
            .map(|c| Cursor::parse(c, AlbumSort::Newest))
            .transpose()?;
        let listed_only = !actor.can_manage(Some(user.id));
        albums::list(
            &self.db,
            user.id,
            None,
            listed_only,
            after,
            clamp_limit(limit),
        )
        .await
        .map_err(database)
    }
}
