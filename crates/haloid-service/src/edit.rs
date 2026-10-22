//! Changing what is already there: a file's details, and albums — create,
//! edit, rearrange, delete — plus the bulk form of the file actions.
//!
//! The rights rule is the one everywhere else uses: the owner, a moderator
//! or an administrator may change a thing, and anyone else is told it does
//! not exist. A file can only be filed into one of its own owner's albums,
//! even when a moderator does the filing.

use crate::files::{DeleteAs, Files};
use crate::settings::SettingsHandle;
use crate::upload::{UploadedFile, Uploads, contains_stop_word};
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::auth::Actor;
use haloid_core::files::{Album, AlbumPrivacy, MediaFile};
use haloid_core::ids::FileId;
use haloid_db::albums::{AlbumDeletion, AlbumDetails, NewAlbum};
use haloid_db::files::FileDetails;
use haloid_db::{Db, albums, files, tags};
use std::sync::Arc;

/// Column limits, in characters: `VARCHAR(255)` for a title,
/// `VARCHAR(190)` for an album name, and a `TEXT` description held under
/// its 64 KB even at four bytes a character.
pub const MAX_TITLE: usize = 255;
pub const MAX_ALBUM_NAME: usize = 190;
pub const MAX_DESCRIPTION: usize = 16_000;
/// The most files one bulk request may name.
pub const MAX_BULK: usize = 100;

/// A partial edit of a file. `None` leaves a field alone; `Some(None)`
/// clears it.
#[derive(Debug, Clone, Default)]
pub struct FileEdit {
    pub title: Option<Option<String>>,
    pub description: Option<Option<String>>,
    pub nsfw: Option<bool>,
    /// The whole new tag list.
    pub tags: Option<Vec<String>>,
    /// An album's public id, or `Some(None)` to take the file out of any.
    pub album: Option<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct AlbumRequest {
    pub name: String,
    pub description: Option<String>,
    pub privacy: AlbumPrivacy,
    /// Required for a password album, ignored for any other.
    pub password: Option<String>,
    /// The parent album's public id.
    pub parent: Option<String>,
}

/// A partial edit of an album, on the same terms as [`FileEdit`].
#[derive(Debug, Clone, Default)]
pub struct AlbumEdit {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub privacy: Option<AlbumPrivacy>,
    /// A new password. Needed when an album becomes a password album, and
    /// replaces the old one when given for an album that already is.
    pub password: Option<String>,
    pub parent: Option<Option<String>>,
    /// The file shown on the album's card, by public id: one of its own.
    /// `Some(None)` goes back to the first picture in the album.
    pub cover: Option<Option<String>>,
}

#[derive(Debug, Clone)]
pub enum BulkAction {
    /// Into an album by public id, or out of any with `None`.
    Move(Option<String>),
    Delete,
}

pub struct Editor {
    db: Db,
    settings: SettingsHandle,
    uploads: Arc<Uploads>,
    files: Files,
}

fn database(error: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Database(Box::new(error))
}

/// Trimmed, empty as absent, and no longer than the column holds.
fn text(field: &str, value: Option<String>, max: usize) -> Result<Option<String>, DomainError> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max {
        return Err(DomainError::invalid_param(
            field,
            format!("is longer than {max} characters"),
        ));
    }
    Ok(Some(value.to_string()))
}

fn album_name(name: &str) -> Result<String, DomainError> {
    text("name", Some(name.to_string()), MAX_ALBUM_NAME)?
        .ok_or_else(|| DomainError::invalid_param("name", "is required"))
}

impl Editor {
    pub fn new(db: Db, settings: SettingsHandle, uploads: Arc<Uploads>, files: Files) -> Self {
        Self {
            db,
            settings,
            uploads,
            files,
        }
    }

    /// A live file the actor may change, or `NotFound`.
    async fn file_to_change(
        &self,
        actor: Actor,
        id_encoded: &str,
        now: DateTime<Utc>,
    ) -> Result<MediaFile, DomainError> {
        let file = files::find_by_id_encoded(&self.db, id_encoded)
            .await
            .map_err(database)?
            .filter(|f| f.is_live(now) && actor.can_manage(f.user_id))
            .ok_or(DomainError::NotFound("file"))?;
        Ok(file)
    }

    /// An album the actor may change, or `NotFound`.
    async fn album_to_change(&self, actor: Actor, id_encoded: &str) -> Result<Album, DomainError> {
        albums::find_by_id_encoded(&self.db, id_encoded)
            .await
            .map_err(database)?
            .filter(|a| actor.can_manage(a.user_id))
            .ok_or(DomainError::NotFound("album"))
    }

    /// The album a file of `owner`'s may be filed into.
    async fn target_album(
        &self,
        actor: Actor,
        owner: Option<haloid_core::ids::UserId>,
        album: Option<&str>,
    ) -> Result<Option<Album>, DomainError> {
        let album = self.uploads.resolve_album(actor, album).await?;
        if let Some(album) = &album
            && (owner.is_none() || album.user_id != owner)
        {
            return Err(DomainError::invalid_param(
                "album",
                "belongs to someone other than the file's owner",
            ));
        }
        Ok(album)
    }

    fn check_stop_words(&self, texts: &[&str]) -> Result<(), DomainError> {
        let settings = self.settings.get();
        if contains_stop_word(&settings.uploads.stop_words, texts) {
            return Err(DomainError::StopWord);
        }
        Ok(())
    }

    /// Change a file's details. Returns the file as it now is.
    pub async fn edit_file(
        &self,
        actor: Actor,
        id_encoded: &str,
        edit: FileEdit,
        now: DateTime<Utc>,
    ) -> Result<UploadedFile, DomainError> {
        let file = self.file_to_change(actor, id_encoded, now).await?;

        let title = match edit.title {
            Some(title) => text("title", title, MAX_TITLE)?,
            None => file.title.clone(),
        };
        let description = match edit.description {
            Some(description) => text("description", description, MAX_DESCRIPTION)?,
            None => file.description.clone(),
        };
        let replace_tags = edit.tags.is_some();
        let tags = edit
            .tags
            .map(|tags| {
                tags.into_iter()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut texts: Vec<&str> = tags.iter().map(String::as_str).collect();
        texts.extend(title.as_deref());
        texts.extend(description.as_deref());
        self.check_stop_words(&texts)?;

        // Resolved before anything is written, so a bad album leaves the
        // file exactly as it was.
        let album = match &edit.album {
            Some(album) => Some(
                self.target_album(actor, file.user_id, album.as_deref())
                    .await?,
            ),
            None => None,
        };

        files::update_details(
            &self.db,
            file.id,
            &FileDetails {
                title,
                description,
                nsfw: edit.nsfw.unwrap_or(file.nsfw),
                category_id: file.category_id,
            },
        )
        .await
        .map_err(database)?;
        if replace_tags {
            let names: Vec<&str> = tags.iter().map(String::as_str).collect();
            tags::set_for_file(&self.db, file.id, &names)
                .await
                .map_err(database)?;
        }
        if let Some(album) = album {
            files::move_to_album(&self.db, file.id, album.map(|a| a.id))
                .await
                .map_err(database)?;
        }
        self.uploads.result(file.id, None, false).await
    }

    async fn password_hash(&self, password: Option<String>) -> Result<String, DomainError> {
        let password = password
            .filter(|p| !p.is_empty())
            .ok_or_else(|| DomainError::invalid_param("password", "a password album needs one"))?;
        tokio::task::spawn_blocking(move || crate::auth::secrets::hash_password(&password))
            .await
            .map_err(|e| DomainError::Internal(e.to_string().into()))?
    }

    async fn parent(
        &self,
        actor: Actor,
        parent: Option<&str>,
    ) -> Result<Option<Album>, DomainError> {
        match parent.filter(|p| !p.is_empty()) {
            Some(parent) => self
                .album_to_change(actor, parent)
                .await
                .map(Some)
                .map_err(|_| DomainError::invalid_param("parent", "no such album")),
            None => Ok(None),
        }
    }

    /// A new album, owned by the actor. Guests have no albums.
    pub async fn create_album(
        &self,
        actor: Actor,
        request: AlbumRequest,
    ) -> Result<Album, DomainError> {
        let owner = actor.user_id().ok_or(DomainError::Unauthenticated)?;
        let name = album_name(&request.name)?;
        let description = text("description", request.description, MAX_DESCRIPTION)?;
        let mut texts = vec![name.as_str()];
        texts.extend(description.as_deref());
        self.check_stop_words(&texts)?;
        let password_hash = match request.privacy {
            AlbumPrivacy::Password => Some(self.password_hash(request.password).await?),
            _ => None,
        };
        let parent = self.parent(actor, request.parent.as_deref()).await?;
        Ok(albums::create(
            &self.db,
            &NewAlbum {
                user_id: Some(owner),
                parent_id: parent.map(|p| p.id),
                name: &name,
                description: description.as_deref(),
                privacy: request.privacy,
                password_hash: password_hash.as_deref(),
            },
        )
        .await?)
    }

    pub async fn edit_album(
        &self,
        actor: Actor,
        id_encoded: &str,
        edit: AlbumEdit,
    ) -> Result<Album, DomainError> {
        let album = self.album_to_change(actor, id_encoded).await?;
        let name = match &edit.name {
            Some(name) => album_name(name)?,
            None => album.name.clone(),
        };
        let description = match edit.description {
            Some(description) => text("description", description, MAX_DESCRIPTION)?,
            None => album.description.clone(),
        };
        let mut texts = vec![name.as_str()];
        texts.extend(description.as_deref());
        self.check_stop_words(&texts)?;

        let privacy = edit.privacy.unwrap_or(album.privacy);
        let password_hash = match privacy {
            AlbumPrivacy::Password if edit.password.is_some() || !album.has_password => {
                Some(self.password_hash(edit.password).await?)
            }
            AlbumPrivacy::Password => albums::password_hash(&self.db, album.id)
                .await
                .map_err(database)?,
            _ => None,
        };
        let parent = match &edit.parent {
            Some(parent) => Some(self.parent(actor, parent.as_deref()).await?),
            None => None,
        };
        if let Some(cover) = &edit.cover {
            let cover = match cover.as_deref() {
                Some(id) => {
                    let file = files::find_by_id_encoded(&self.db, id)
                        .await
                        .map_err(database)?
                        .filter(|f| f.album_id == Some(album.id) && !f.is_deleted())
                        .ok_or_else(|| {
                            DomainError::invalid_param("cover", "is not a file in this album")
                        })?;
                    Some(file.id)
                }
                None => None,
            };
            albums::set_cover(&self.db, album.id, cover)
                .await
                .map_err(database)?;
        }
        if let Some(parent) = &parent {
            // Owner-checked and cycle-checked in the repository.
            albums::set_parent(&self.db, album.id, parent.as_ref().map(|p| p.id)).await?;
        }
        albums::update(
            &self.db,
            album.id,
            &AlbumDetails {
                name,
                description,
                privacy,
                password_hash,
            },
        )
        .await
        .map_err(database)?;
        albums::find_by_id(&self.db, album.id)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("album"))
    }

    /// Delete an album. Its files are either kept, loose, or deleted with
    /// it — retained and quarantined like any other deletion.
    pub async fn delete_album(
        &self,
        actor: Actor,
        id_encoded: &str,
        with_contents: bool,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        let album = self.album_to_change(actor, id_encoded).await?;
        let mode = if with_contents {
            AlbumDeletion::WithContents {
                by: actor.user_id(),
                at: now,
            }
        } else {
            AlbumDeletion::KeepContents
        };
        let deleted = albums::delete(&self.db, album.id, mode)
            .await
            .map_err(database)?
            .ok_or(DomainError::NotFound("album"))?;
        self.files.quarantine_ids(&deleted).await
    }

    /// Put an album's files in the order given; files left out follow in
    /// their current order, and ids from elsewhere are ignored.
    pub async fn reorder_album(
        &self,
        actor: Actor,
        id_encoded: &str,
        order: &[String],
    ) -> Result<(), DomainError> {
        let album = self.album_to_change(actor, id_encoded).await?;
        if order.len() > 10_000 {
            return Err(DomainError::invalid_param("files", "too many files"));
        }
        let mut ids: Vec<FileId> = Vec::with_capacity(order.len());
        for id in order {
            if let Some(file) = files::find_by_id_encoded(&self.db, id)
                .await
                .map_err(database)?
                .filter(|f| f.album_id == Some(album.id))
            {
                ids.push(file.id);
            }
        }
        files::set_album_order(&self.db, album.id, &ids)
            .await
            .map_err(database)
    }

    /// One action over many files, each succeeding or failing alone: a file
    /// the actor may not touch fails as `NotFound` without stopping the rest.
    pub async fn bulk(
        &self,
        actor: Actor,
        ids: &[String],
        action: BulkAction,
        now: DateTime<Utc>,
    ) -> Result<Vec<(String, Result<(), DomainError>)>, DomainError> {
        if ids.is_empty() {
            return Err(DomainError::invalid_param("ids", "names no files"));
        }
        if ids.len() > MAX_BULK {
            return Err(DomainError::invalid_param(
                "ids",
                format!("names more than {MAX_BULK} files"),
            ));
        }
        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            let outcome = match &action {
                BulkAction::Move(album) => self
                    .edit_file(
                        actor,
                        id,
                        FileEdit {
                            album: Some(album.clone()),
                            ..FileEdit::default()
                        },
                        now,
                    )
                    .await
                    .map(|_| ()),
                BulkAction::Delete => self.files.delete(id, DeleteAs::Actor(actor), now).await,
            };
            results.push((id.clone(), outcome));
        }
        Ok(results)
    }
}
