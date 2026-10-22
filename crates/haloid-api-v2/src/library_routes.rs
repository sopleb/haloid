//! Reading the library: explore, profiles, albums, and the client config.

use crate::caller::{album_grants, identify};
use crate::problem::Problem;
use crate::views::{AlbumView, ConfigView, FileView, PageView, UploadConfig, UserView};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Json;
use chrono::Utc;
use haloid_core::DomainError;
use haloid_core::media::MediaKind;
use haloid_core::pagination::{FileSort, SortOrder};
use haloid_service::app::App;
use haloid_service::library::ListRequest;
use serde::Deserialize;
use std::sync::Arc;

/// `?sort=newest|oldest|views|likes|manual&cursor=…&limit=…&kind=image`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct ListParams {
    pub sort: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub kind: Option<String>,
}

impl ListParams {
    fn request(&self) -> Result<ListRequest, DomainError> {
        let sort = match self.sort.as_deref() {
            None | Some("") => None,
            Some("newest") => Some(FileSort::Newest),
            Some("oldest") => Some(FileSort::Oldest),
            Some("views") => Some(FileSort::MostViewed),
            Some("likes") => Some(FileSort::MostLiked),
            Some("manual") => Some(FileSort::AlbumOrder),
            Some(_) => {
                return Err(DomainError::invalid_param(
                    "sort",
                    "one of newest, oldest, views, likes, manual",
                ));
            }
        };
        let kind =
            match self.kind.as_deref() {
                None | Some("") => None,
                Some(kind) => Some(kind.parse::<MediaKind>().map_err(|()| {
                    DomainError::invalid_param("kind", "one of image, video, audio")
                })?),
            };
        // A cursor for one ordering cannot resume another; the code in
        // the cursor says which it was made for.
        let sort = sort.or_else(|| {
            self.cursor
                .as_deref()
                .and_then(|c| c.chars().next())
                .and_then(FileSort::from_code)
        });
        Ok(ListRequest {
            sort,
            cursor: self.cursor.clone(),
            limit: self.limit,
            kind,
        })
    }
}

/// `GET /api/v2/files`: the public gallery.
#[utoipa::path(get, path = "/api/v2/files", tag = "library", params(ListParams),
    responses(
        (status = 200, body = PageView<FileView>),
        (status = 404, description = "Not there, or not for this caller", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad parameters", body = crate::problem::ProblemBody),
    ))]
pub async fn explore(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> Result<Json<PageView<FileView>>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    let page = app
        .library
        .explore(caller.actor, &params.request()?, now)
        .await?;
    Ok(Json(PageView::map(page, |f| {
        FileView::listed(f, caller.actor)
    })))
}

/// `GET /api/v2/users/{username}`.
#[utoipa::path(get, path = "/api/v2/users/{username}", tag = "library", params(("username" = String, Path)),
    responses(
        (status = 200, body = UserView),
        (status = 404, description = "Not there, or not for this caller", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad parameters", body = crate::problem::ProblemBody),
    ))]
pub async fn user(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<Json<UserView>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let user = app.library.user(caller.actor, &username).await?;
    Ok(Json(UserView::new(&user)))
}

/// `GET /api/v2/users/{username}/files`.
#[utoipa::path(get, path = "/api/v2/users/{username}/files", tag = "library", params(("username" = String, Path), ListParams),
    responses(
        (status = 200, body = PageView<FileView>),
        (status = 404, description = "Not there, or not for this caller", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad parameters", body = crate::problem::ProblemBody),
    ))]
pub async fn user_files(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Query(params): Query<ListParams>,
) -> Result<Json<PageView<FileView>>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    let page = app
        .library
        .user_files(caller.actor, &username, &params.request()?, now)
        .await?;
    Ok(Json(PageView::map(page, |f| {
        FileView::listed(f, caller.actor)
    })))
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct AlbumListParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// `GET /api/v2/users/{username}/albums`: top-level albums.
#[utoipa::path(get, path = "/api/v2/users/{username}/albums", tag = "library", params(("username" = String, Path), AlbumListParams),
    responses(
        (status = 200, body = PageView<AlbumView>),
        (status = 404, description = "Not there, or not for this caller", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad parameters", body = crate::problem::ProblemBody),
    ))]
pub async fn user_albums(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Query(params): Query<AlbumListParams>,
) -> Result<Json<PageView<AlbumView>>, Problem> {
    let caller = identify(&app, &headers, Utc::now()).await?;
    let page = app
        .library
        .user_albums(
            caller.actor,
            &username,
            params.cursor.as_deref(),
            params.limit,
        )
        .await?;
    let now = Utc::now();
    let mut covers = Vec::with_capacity(page.items.len());
    for album in &page.items {
        covers.push(app.library.cover(caller.actor, album, now).await?);
    }
    Ok(Json(PageView {
        items: page
            .items
            .iter()
            .zip(covers)
            .map(|(a, cover)| AlbumView::new(a, caller.actor).with_cover(cover.as_ref()))
            .collect(),
        next: page.next.map(|c| c.to_string()),
    }))
}

/// `GET /api/v2/albums/{id}`.
#[utoipa::path(get, path = "/api/v2/albums/{id}", tag = "library", params(("id" = String, Path)),
    responses(
        (status = 200, body = AlbumView),
        (status = 404, description = "Not there, or not for this caller", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad parameters", body = crate::problem::ProblemBody),
    ))]
pub async fn album(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<AlbumView>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    let unlocked = app.album_locks.unlocked(&album_grants(&headers), now).await;
    let album = app.library.album_for(caller.actor, &id, &unlocked).await?;
    let cover = app.library.cover(caller.actor, &album, now).await?;
    Ok(Json(
        AlbumView::new(&album, caller.actor).with_cover(cover.as_ref()),
    ))
}

/// `GET /api/v2/albums/{id}/files`, in the owner's order by default.
#[utoipa::path(get, path = "/api/v2/albums/{id}/files", tag = "library", params(("id" = String, Path), ListParams),
    responses(
        (status = 200, body = PageView<FileView>),
        (status = 404, description = "Not there, or not for this caller", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad parameters", body = crate::problem::ProblemBody),
    ))]
pub async fn album_files(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<ListParams>,
) -> Result<Json<PageView<FileView>>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    let page = app
        .library
        .album_files_for(
            caller.actor,
            &id,
            &params.request()?,
            now,
            &app.album_locks.unlocked(&album_grants(&headers), now).await,
        )
        .await?;
    Ok(Json(PageView::map(page, |f| {
        FileView::listed(f, caller.actor)
    })))
}

/// `GET /api/v2/config`: what this caller may do, before a byte leaves.
#[utoipa::path(get, path = "/api/v2/config", tag = "config", 
    responses(
        (status = 200, body = ConfigView),
        (status = 404, description = "Not there, or not for this caller", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad parameters", body = crate::problem::ProblemBody),
    ))]
pub async fn config(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<ConfigView>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    let settings = app.settings.get();
    let entitled = app.licensing.entitlements(now);
    // Audio is Pro: a Community instance does not offer what it would
    // refuse.
    let extensions = settings
        .uploads
        .allowed_extensions
        .iter()
        .filter(|e| entitled.pro || MediaKind::of_extension(e) != Some(MediaKind::Audio))
        .cloned()
        .collect();
    Ok(Json(ConfigView {
        site_name: settings.site.name.clone(),
        base_url: settings.site.base_url.clone(),
        registration_open: settings.site.registration_open,
        signed_in: !caller.actor.is_guest(),
        theme: settings.site.theme.clone(),
        branding: entitled.branding,
        uploads: UploadConfig {
            allowed: settings.may_upload(caller.actor),
            guests_allowed: settings.uploads.enabled && settings.uploads.guests_enabled,
            max_size: settings.max_upload_size(caller.actor),
            max_pixels: settings.uploads.max_pixels,
            extensions,
            max_expiration_secs: settings.uploads.max_expiration.as_secs(),
            url_sources: settings.uploads.allow_url_sources,
        },
    }))
}
