//! The OpenAPI 3.1 document for `/api/v2`, served at
//! `/api/v2/openapi.json` and committed as
//! `frontend/src/lib/api/openapi.json`, from which the frontend's types are
//! generated. A test fails when the committed copy drifts from the code, so
//! a backend change that would break the frontend breaks the build instead.

use utoipa::OpenApi;

/// The multipart upload form, for the document.
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
pub struct UploadForm {
    /// The file itself.
    #[schema(value_type = String, format = Binary)]
    pub file: Option<Vec<u8>>,
    /// Instead of `file`: a URL to fetch it from, where the instance allows
    /// URL uploads (`uploads.url_sources` in `/api/v2/config`).
    pub url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// Comma-separated, or the field repeated.
    pub tags: Option<String>,
    /// An album's public id.
    pub album: Option<String>,
    pub nsfw: Option<bool>,
    /// Seconds until the file expires.
    pub expires_in: Option<u64>,
    /// Shrink the served image to this width, keeping the original.
    pub width: Option<u32>,
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Haloid API",
        description = "Haloid's own REST API. Errors are RFC 9457 problem details; dates are RFC 3339; lists are cursor-paginated."
    ),
    paths(
        crate::routes::upload,
        crate::routes::file,
        crate::routes::view,
        crate::session_routes::open,
        crate::session_routes::put_chunk,
        crate::session_routes::state,
        crate::session_routes::complete,
        crate::session_routes::abandon,
        crate::write_routes::edit_file,
        crate::write_routes::delete_file,
        crate::write_routes::bulk,
        crate::write_routes::create_album,
        crate::write_routes::edit_album,
        crate::write_routes::delete_album,
        crate::write_routes::reorder_album,
        crate::admin_routes::test_storage,
        crate::admin_routes::storages,
        crate::admin_routes::create_storage,
        crate::admin_routes::update_storage,
        crate::admin_routes::retire_storage,
        crate::admin_routes::moderation_queue,
        crate::admin_routes::approve,
        crate::admin_routes::deny,
        crate::admin_routes::settings,
        crate::admin_routes::update_settings,
        crate::admin_routes::users,
        crate::admin_routes::update_user,
        crate::admin_routes::deleted_files,
        crate::admin_routes::restore_file,
        crate::admin_routes::purge_file,
        crate::admin_routes::audit_log,
        crate::admin_routes::metrics,
        crate::admin_routes::watermark,
        crate::admin_routes::set_watermark,
        crate::admin_routes::remove_watermark,
        crate::admin_routes::licence,
        crate::admin_routes::updates,
        crate::admin_routes::check_updates,
        crate::admin_routes::install_update,
        crate::admin_routes::install_licence,
        crate::admin_routes::remove_licence,
        crate::library_routes::explore,
        crate::library_routes::user,
        crate::library_routes::user_files,
        crate::library_routes::user_albums,
        crate::library_routes::album,
        crate::library_routes::album_files,
        crate::library_routes::config,
        crate::auth_routes::challenge,
        crate::auth_routes::register,
        crate::auth_routes::login,
        crate::auth_routes::logout,
        crate::auth_routes::me,
        crate::account_routes::update_me,
        crate::account_routes::change_password,
        crate::account_routes::api_keys,
        crate::account_routes::create_api_key,
        crate::account_routes::revoke_api_key,
        crate::account_routes::sessions,
        crate::account_routes::revoke_session,
        crate::account_routes::unlock_album,
    ),
    components(schemas(crate::problem::ProblemBody, UploadForm)),
    tags(
        (name = "files", description = "Uploading and reading files"),
        (name = "library", description = "Lists: explore, profiles, albums"),
        (name = "albums", description = "Creating and arranging albums"),
        (name = "admin", description = "Administering the instance"),
        (name = "account", description = "Your own account: profile, password, keys, sessions"),
        (name = "auth", description = "Signing in, for the browser"),
        (name = "config", description = "What this caller may do"),
    )
)]
pub struct ApiDoc;

/// The document as committed: pretty, with a trailing newline.
pub fn document() -> String {
    let mut json = ApiDoc::openapi()
        .to_pretty_json()
        .expect("the OpenAPI document serialises");
    json.push('\n');
    json
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// `HALOID_UPDATE_OPENAPI=1 cargo test -p haloid-api-v2 openapi` rewrites
    /// the committed copy; then run `pnpm api:types` in `frontend/`.
    #[test]
    fn the_committed_openapi_document_is_current() {
        let committed =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontend/src/lib/api/openapi.json");
        let current = document();
        if std::env::var_os("HALOID_UPDATE_OPENAPI").is_some() {
            std::fs::write(&committed, &current).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&committed).unwrap_or_default();
        assert!(
            on_disk == current,
            "frontend/src/lib/api/openapi.json is stale: run \
             HALOID_UPDATE_OPENAPI=1 cargo test -p haloid-api-v2 openapi, \
             then pnpm api:types in frontend/"
        );
    }

    #[test]
    fn the_document_is_openapi_3_1_with_every_route() {
        let doc: serde_json::Value = serde_json::from_str(&document()).unwrap();
        assert!(doc["openapi"].as_str().unwrap().starts_with("3.1"));
        let paths = doc["paths"].as_object().unwrap();
        for path in [
            "/api/v2/uploads",
            "/api/v2/files/{id}",
            "/api/v2/auth/login",
            "/api/v2/config",
        ] {
            assert!(paths.contains_key(path), "{path} is undocumented");
        }
    }
}
