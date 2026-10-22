//! What v2 returns: real JSON types, RFC 3339 dates, `chain` as derivative
//! names. Nothing here should require a client to know legacy trivia.

use chrono::{DateTime, Utc};
use haloid_core::account::User;
use haloid_core::auth::Actor;
use haloid_core::files::{Album, MediaFile};
use haloid_core::media::MediaKind;
use haloid_core::pagination::{Page, SortOrder};
use haloid_service::library::Listed;
use haloid_service::upload::{FileUrls as ServiceUrls, UploadedFile};
use serde::Serialize;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct FileUrls {
    pub viewer: String,
    pub image: Option<String>,
    pub thumb: Option<String>,
    pub medium: Option<String>,
    pub frame: Option<String>,
    /// The one-shot delete link. Only in the response to the upload that
    /// created the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,
}

/// What sort of file it is.
#[derive(Debug, Clone, Copy, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Video,
    Audio,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct FileView {
    pub id: String,
    pub kind: Kind,
    pub mime: String,
    pub extension: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// Display dimensions, after rotation. Zero for audio.
    pub width: u32,
    pub height: u32,
    pub size: u64,
    pub duration_ms: Option<u32>,
    pub is_animated: bool,
    pub nsfw: bool,
    pub approved: bool,
    /// Which derivatives exist: `["image", "medium", "thumb"]`.
    pub chain: Vec<&'static str>,
    pub tags: Vec<String>,
    pub album: Option<String>,
    /// The owner's username, on a single file; `null` for a guest's upload
    /// and in lists.
    pub owner: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    /// Views of its page, one per viewer per half hour; the owner's own
    /// views are not counted.
    pub views: u64,
    pub urls: FileUrls,
    /// Whether this caller may edit and delete the file: its owner, a
    /// moderator or an administrator.
    pub can_edit: bool,
}

impl FileView {
    pub fn new(uploaded: &UploadedFile, with_delete: bool, caller: Actor) -> Self {
        Self::build(
            &uploaded.file,
            &uploaded.urls,
            uploaded.tags.iter().map(|t| t.name.clone()).collect(),
            uploaded.album.as_ref().map(|a| a.id_encoded.to_string()),
            uploaded.owner.clone(),
            with_delete,
            caller,
        )
    }

    /// A list item: no tags (a list is not where they are read) and no
    /// album.
    pub fn listed(listed: &Listed, caller: Actor) -> Self {
        Self::build(
            &listed.file,
            &listed.urls,
            Vec::new(),
            None,
            None,
            false,
            caller,
        )
    }

    fn build(
        file: &MediaFile,
        urls: &ServiceUrls,
        tags: Vec<String>,
        album: Option<String>,
        owner: Option<String>,
        with_delete: bool,
        caller: Actor,
    ) -> Self {
        let url = |key: &str| urls.derivatives.get(key).cloned();
        Self {
            id: file.id_encoded.to_string(),
            kind: match file.kind {
                MediaKind::Image => Kind::Image,
                MediaKind::Video => Kind::Video,
                MediaKind::Audio => Kind::Audio,
            },
            mime: file.mime.clone(),
            extension: file.extension.clone(),
            title: file.title.clone(),
            description: file.description.clone(),
            width: file.width,
            height: file.height,
            size: file.size,
            duration_ms: file.duration_ms,
            is_animated: file.is_animated,
            nsfw: file.nsfw,
            approved: file.is_approved,
            chain: file.chain.to_labels(),
            tags,
            album,
            owner,
            created_at: file.created_at,
            expires_at: file.expires_at,
            views: file.view_count,
            urls: FileUrls {
                viewer: urls.viewer.clone(),
                image: url("image"),
                thumb: url("thumb"),
                medium: url("medium"),
                frame: url("frame"),
                delete: if with_delete {
                    urls.delete.clone()
                } else {
                    None
                },
            },
            can_edit: caller.can_manage(file.user_id),
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UploadResponse {
    pub file: FileView,
    /// Whether an earlier upload of the same bytes was returned instead.
    pub duplicate: bool,
}

/// One page of a list. `next` is the cursor for the page after, `null` on
/// the last.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PageView<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

impl<T> PageView<T> {
    pub fn map<U, S: SortOrder>(page: Page<U, S>, f: impl Fn(&U) -> T) -> Self {
        Self {
            items: page.items.iter().map(f).collect(),
            next: page.next.map(|c| c.to_string()),
        }
    }
}

/// A user as anyone may see them.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UserView {
    pub username: String,
    pub display_name: Option<String>,
    pub file_count: u32,
    pub album_count: u32,
    pub created_at: DateTime<Utc>,
}

impl UserView {
    pub fn new(user: &User) -> Self {
        Self {
            username: user.username.clone(),
            display_name: user.display_name.clone(),
            file_count: user.file_count,
            album_count: user.album_count,
            created_at: user.created_at,
        }
    }
}

/// The signed-in user, as they see themselves.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MeView {
    pub id: String,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub role: &'static str,
    /// Whether the profile is hidden from everyone else.
    pub is_private: bool,
    pub file_count: u32,
    pub album_count: u32,
    pub disk_used: u64,
    pub created_at: DateTime<Utc>,
}

impl MeView {
    pub fn new(user: &User) -> Self {
        Self {
            id: user.id_encoded.to_string(),
            username: user.username.clone(),
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            role: user.role.as_str(),
            is_private: user.is_private,
            file_count: user.file_count,
            album_count: user.album_count,
            disk_used: user.disk_used,
            created_at: user.created_at,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AlbumView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub privacy: &'static str,
    pub file_count: u32,
    pub created_at: DateTime<Utc>,
    /// Whether this caller may edit, rearrange and delete the album.
    pub can_edit: bool,
    /// A small picture for the album's card: its chosen cover, else its
    /// first picture or video. `null` for an album with nothing to show.
    pub cover: Option<String>,
}

impl AlbumView {
    pub fn new(album: &Album, caller: Actor) -> Self {
        Self {
            id: album.id_encoded.to_string(),
            name: album.name.clone(),
            description: album.description.clone(),
            privacy: album.privacy.as_str(),
            file_count: album.file_count,
            created_at: album.created_at,
            can_edit: caller.can_manage(album.user_id),
            cover: None,
        }
    }

    /// With the cover's thumbnail, or the best picture it has.
    pub fn with_cover(mut self, cover: Option<&ServiceUrls>) -> Self {
        self.cover = cover.and_then(|urls| {
            ["thumb", "medium", "frame", "image"]
                .iter()
                .find_map(|key| urls.derivatives.get(*key).cloned())
        });
        self
    }
}

/// What the frontend needs before a byte leaves: whether this caller may
/// upload, how much, and what.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ConfigView {
    pub site_name: String,
    pub base_url: String,
    pub registration_open: bool,
    pub signed_in: bool,
    /// The site's default theme, for a visitor who has not picked one.
    pub theme: String,
    /// Whether "Powered by Haloid" is shown: true unless a licence removes
    /// it.
    pub branding: bool,
    pub uploads: UploadConfig,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UploadConfig {
    /// Whether this caller may upload at all.
    pub allowed: bool,
    pub guests_allowed: bool,
    /// This caller's limit, in bytes.
    pub max_size: u64,
    pub max_pixels: u64,
    pub extensions: Vec<String>,
    pub max_expiration_secs: u64,
    pub url_sources: bool,
}
