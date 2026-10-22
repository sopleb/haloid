//! Changing things: edit and delete a file, the bulk forms of both, and
//! albums — create, edit, reorder, delete.
//!
//! Every handler here is an unsafe method, so a cookie caller passes the
//! CSRF rule first. A bearer caller needs no CSRF check; a guest with a
//! delete token needs none either, since the token is the authority.

use crate::auth_routes::body;
use crate::caller::{check_csrf, identify};
use crate::problem::{Problem, classify};
use crate::views::{AlbumView, FileView};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use haloid_core::DomainError;
use haloid_core::files::AlbumPrivacy;
use haloid_service::app::App;
use haloid_service::edit::{AlbumEdit, AlbumRequest, BulkAction, FileEdit};
use haloid_service::files::DeleteAs;
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;

/// For a PATCH body: an absent field is `None`, an explicit `null` is
/// `Some(None)`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn privacy(value: Option<&str>) -> Result<Option<AlbumPrivacy>, DomainError> {
    value
        .map(|p| {
            p.parse().map_err(|()| {
                DomainError::invalid_param("privacy", "one of public, unlisted, private, password")
            })
        })
        .transpose()
}

/// Fields to change on a file. Leave a field out to keep it; send `null`
/// to clear a title, description or album.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FilePatch {
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable)]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable)]
    pub description: Option<Option<String>>,
    pub nsfw: Option<bool>,
    /// The complete new tag list.
    pub tags: Option<Vec<String>>,
    /// An album id of the file owner's, or `null` for none.
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable)]
    pub album: Option<Option<String>>,
}

/// `PATCH /api/v2/files/{id}`.
#[utoipa::path(patch, path = "/api/v2/files/{id}", tag = "files",
    request_body = FilePatch,
    responses(
        (status = 200, description = "The file as it now is", body = FileView),
        (status = 404, description = "No such file, or not this caller's to change", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad fields, or a stop word", body = crate::problem::ProblemBody),
    ))]
pub async fn edit_file(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
    patch: Result<Json<FilePatch>, JsonRejection>,
) -> Result<Json<FileView>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let patch = body(patch)?;
    let edited = app
        .editor
        .edit_file(
            caller.actor,
            &id,
            FileEdit {
                title: patch.title,
                description: patch.description,
                nsfw: patch.nsfw,
                tags: patch.tags,
                album: patch.album,
            },
            now,
        )
        .await?;
    Ok(Json(FileView::new(&edited, false, caller.actor)))
}

/// `DELETE /api/v2/files/{id}`: as the owner, a moderator or an admin, or —
/// for anyone — with the upload's delete token in `X-Delete-Token`. The file
/// is retained for audit and its bytes quarantined; only an administrator
/// can restore or purge it.
#[utoipa::path(delete, path = "/api/v2/files/{id}", tag = "files",
    params(("X-Delete-Token" = Option<String>, Header, description = "The token from the upload's delete link")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such file, or not this caller's to delete", body = crate::problem::ProblemBody),
    ))]
pub async fn delete_file(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<StatusCode, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let token = headers
        .get("x-delete-token")
        .and_then(|v| v.to_str().ok())
        .filter(|t| !t.is_empty());
    let authority = match token {
        Some(token) => DeleteAs::Token(token),
        None => DeleteAs::Actor(caller.actor),
    };
    app.files.delete(&id, authority, now).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum BulkVerb {
    Move,
    Delete,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BulkBody {
    pub action: BulkVerb,
    /// File ids, at most 100.
    pub ids: Vec<String>,
    /// For `move`: the album, or `null` for none.
    #[serde(default)]
    pub album: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BulkFailure {
    pub id: String,
    /// The problem code it would have had alone: `not_found`, …
    pub code: &'static str,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BulkResult {
    pub done: Vec<String>,
    pub failed: Vec<BulkFailure>,
}

/// `POST /api/v2/files/bulk`: one action over many files. Each file
/// succeeds or fails on its own; the response says which.
#[utoipa::path(post, path = "/api/v2/files/bulk", tag = "files",
    request_body = BulkBody,
    responses(
        (status = 200, description = "What happened to each file", body = BulkResult),
        (status = 422, description = "No ids, too many, or a bad body", body = crate::problem::ProblemBody),
    ))]
pub async fn bulk(
    State(app): State<Arc<App>>,
    method: Method,
    headers: HeaderMap,
    request: Result<Json<BulkBody>, JsonRejection>,
) -> Result<Json<BulkResult>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let request = body(request)?;
    let action = match request.action {
        BulkVerb::Move => BulkAction::Move(request.album),
        BulkVerb::Delete => BulkAction::Delete,
    };
    let outcomes = app
        .editor
        .bulk(caller.actor, &request.ids, action, now)
        .await?;
    let mut result = BulkResult {
        done: Vec::new(),
        failed: Vec::new(),
    };
    for (id, outcome) in outcomes {
        match outcome {
            Ok(()) => result.done.push(id),
            Err(error) => {
                if !error.is_client_safe() {
                    tracing::error!(%id, error = %error, "bulk action failed");
                }
                result.failed.push(BulkFailure {
                    id,
                    code: classify(&error).1,
                });
            }
        }
    }
    Ok(Json(result))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewAlbumBody {
    pub name: String,
    pub description: Option<String>,
    /// `public` (the default), `unlisted`, `private` or `password`.
    pub privacy: Option<String>,
    /// Required for a password album.
    pub password: Option<String>,
    /// A parent album's id, for a nested album.
    pub parent: Option<String>,
}

/// `POST /api/v2/albums`: a new album, owned by the caller.
#[utoipa::path(post, path = "/api/v2/albums", tag = "albums",
    request_body = NewAlbumBody,
    responses(
        (status = 201, description = "Created", body = AlbumView),
        (status = 401, description = "Guests have no albums", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad fields, a bad parent, or nested too deep", body = crate::problem::ProblemBody),
    ))]
pub async fn create_album(
    State(app): State<Arc<App>>,
    method: Method,
    headers: HeaderMap,
    request: Result<Json<NewAlbumBody>, JsonRejection>,
) -> Result<Response, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let request = body(request)?;
    let album = app
        .editor
        .create_album(
            caller.actor,
            AlbumRequest {
                name: request.name,
                description: request.description,
                privacy: privacy(request.privacy.as_deref())?.unwrap_or(AlbumPrivacy::Public),
                password: request.password,
                parent: request.parent,
            },
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(AlbumView::new(&album, caller.actor)),
    )
        .into_response())
}

/// Fields to change on an album. Leave a field out to keep it; send `null`
/// to clear a description or move the album to the top level.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AlbumPatch {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable)]
    pub description: Option<Option<String>>,
    pub privacy: Option<String>,
    /// Needed to make an album a password album; replaces the password of
    /// one that already is.
    pub password: Option<String>,
    /// A new parent album, or `null` for the top level.
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable)]
    pub parent: Option<Option<String>>,
    /// The file to show on the album's card, one of its own; `null` goes
    /// back to its first picture.
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable)]
    pub cover: Option<Option<String>>,
}

/// `PATCH /api/v2/albums/{id}`.
#[utoipa::path(patch, path = "/api/v2/albums/{id}", tag = "albums",
    request_body = AlbumPatch,
    responses(
        (status = 200, description = "The album as it now is", body = AlbumView),
        (status = 404, description = "No such album, or not this caller's to change", body = crate::problem::ProblemBody),
        (status = 422, description = "Bad fields, or a move into itself or too deep", body = crate::problem::ProblemBody),
    ))]
pub async fn edit_album(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
    patch: Result<Json<AlbumPatch>, JsonRejection>,
) -> Result<Json<AlbumView>, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let patch = body(patch)?;
    let album = app
        .editor
        .edit_album(
            caller.actor,
            &id,
            AlbumEdit {
                name: patch.name,
                description: patch.description,
                privacy: privacy(patch.privacy.as_deref())?,
                password: patch.password,
                parent: patch.parent,
                cover: patch.cover,
            },
        )
        .await?;
    let cover = app.library.cover(caller.actor, &album, now).await?;
    Ok(Json(
        AlbumView::new(&album, caller.actor).with_cover(cover.as_ref()),
    ))
}

/// `?contents=keep|delete`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct DeleteAlbumParams {
    /// `keep` leaves the files, out of any album, and moves sub-albums up;
    /// `delete` deletes the files and sub-albums too. Required: there is no
    /// safe guess.
    pub contents: String,
}

/// `DELETE /api/v2/albums/{id}?contents=keep|delete`.
#[utoipa::path(delete, path = "/api/v2/albums/{id}", tag = "albums",
    params(DeleteAlbumParams),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such album, or not this caller's to delete", body = crate::problem::ProblemBody),
        (status = 422, description = "`contents` missing or not keep/delete", body = crate::problem::ProblemBody),
    ))]
pub async fn delete_album(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
    params: Result<Query<DeleteAlbumParams>, axum::extract::rejection::QueryRejection>,
) -> Result<StatusCode, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let with_contents = match params.as_ref().map(|Query(p)| p.contents.as_str()) {
        Ok("keep") => false,
        Ok("delete") => true,
        _ => {
            return Err(DomainError::invalid_param("contents", "keep or delete").into());
        }
    };
    app.editor
        .delete_album(caller.actor, &id, with_contents, now)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OrderBody {
    /// File ids in their new order. Files left out follow, in their current
    /// order; ids not in the album are ignored.
    pub files: Vec<String>,
}

/// `PUT /api/v2/albums/{id}/order`.
#[utoipa::path(put, path = "/api/v2/albums/{id}/order", tag = "albums",
    request_body = OrderBody,
    responses(
        (status = 204, description = "Rearranged"),
        (status = 404, description = "No such album, or not this caller's to change", body = crate::problem::ProblemBody),
    ))]
pub async fn reorder_album(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
    order: Result<Json<OrderBody>, JsonRejection>,
) -> Result<StatusCode, Problem> {
    let now = Utc::now();
    let caller = identify(&app, &headers, now).await?;
    check_csrf(
        &caller,
        &method,
        &headers,
        &app.settings.get().site.base_url,
    )?;
    let order = body(order)?;
    app.editor
        .reorder_album(caller.actor, &id, &order.files)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
