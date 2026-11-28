//! Sonarr v3 compatibility API mounted at `/sonarr/api/v3`
//! (docs/14-compatibility.md, section D "Sonarr endpoints (M8)").
//!
//! Shared endpoints (`system/status`, `qualityprofile`, `rootfolder`,
//! `languageprofile`, and `tag`) are served here, the series read endpoints
//! live in [`crate::sonarr_series`], and the episode endpoints live in
//! [`crate::sonarr_episodes`]. Series add, update, delete and lookup, plus
//! `queue` and `command`, await the shows CRUD layer (#88) and downloads queue
//! (#63) and fall through to the logged `501 Not Implemented` fallback.

use std::path::Path;

use arr_compat::sonarr::dto::{
    LanguageProfileResource, QualityProfileResource, RootFolderResource, SystemResource,
    TagResource,
};
use arr_compat::sonarr::map::{self, MapError, ProfileView, RootFolderView, SystemView};
use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::{Method, StatusCode};
use db::tags::TagError;
use serde_json::json;
use unseenarr_core::permission::Permission;
use unseenarr_core::profile::Item;
use unseenarr_core::root_folder;

use crate::AppState;
use crate::access::{LibraryEdit, LibraryRead, ProfilesRead, SettingsRead};
use crate::audit::{Actor, ClientIp, Event, record};
use crate::compat::{CompatError, CompatRouter, RequireApiKey};

/// Mount prefix for the Sonarr v3 compatibility router.
pub const API_BASE: &str = "/sonarr/api/v3";

/// Every Sonarr v3 compatibility route is registered here with its required
/// permission. Routes are served case-insensitively with API-key-only auth;
/// anything unimplemented falls back to a logged `501 Not Implemented` JSON
/// response.
#[must_use]
pub fn routes() -> CompatRouter {
    CompatRouter::with_base(API_BASE)
        .require(
            Permission::SettingsRead,
            Method::GET,
            "/system/status",
            system_status,
        )
        .require(
            Permission::ProfilesRead,
            Method::GET,
            "/qualityprofile",
            list_quality_profiles,
        )
        .require(
            Permission::SettingsRead,
            Method::GET,
            "/rootfolder",
            list_root_folders,
        )
        .require(
            Permission::ProfilesRead,
            Method::GET,
            "/languageprofile",
            list_language_profiles,
        )
        .require(
            Permission::LibraryRead,
            Method::GET,
            "/series",
            crate::sonarr_series::list_series,
        )
        .require(
            Permission::LibraryRead,
            Method::GET,
            "/series/lookup",
            crate::sonarr_series::lookup_series,
        )
        .require(
            Permission::LibraryRead,
            Method::GET,
            "/series/{id}",
            crate::sonarr_series::get_series,
        )
        .require(
            Permission::LibraryRead,
            Method::GET,
            "/episode",
            crate::sonarr_episodes::list_episodes,
        )
        .require(
            Permission::LibraryEdit,
            Method::PUT,
            "/episode/monitor",
            crate::sonarr_episodes::monitor_episodes,
        )
        .require(Permission::LibraryRead, Method::GET, "/tag", list_tags)
        .require(Permission::LibraryEdit, Method::POST, "/tag", create_tag)
        .require(
            Permission::LibraryEdit,
            Method::PUT,
            "/tag/{id}",
            update_tag,
        )
}

fn failed(context: &'static str, error: impl std::fmt::Display) -> CompatError {
    tracing::error!(%error, context, "sonarr compat");
    CompatError::internal()
}

fn parse_tag_label(resource: &TagResource) -> Result<String, CompatError> {
    let raw = map::tag_label(resource).map_err(|err| match err {
        MapError::Missing(field) => CompatError::validation(field, "must not be empty"),
        MapError::Invalid(field, msg) => CompatError::validation(field, msg),
    })?;
    let label = raw.to_ascii_lowercase();
    if label.len() > 64 {
        return Err(CompatError::validation(
            "label",
            "must have 1 to 64 characters",
        ));
    }
    if !label
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(CompatError::validation(
            "label",
            "may hold only letters, digits and `-`",
        ));
    }
    Ok(label)
}

fn tag_error(error: TagError) -> CompatError {
    match error {
        TagError::NotFound => CompatError::not_found(),
        TagError::LabelTaken => {
            CompatError::validation("label", "A tag with this label already exists")
        }
        TagError::InUse => CompatError::validation("id", "Tag is currently in use"),
        TagError::Db(err) => failed("tags", err),
    }
}

/// `GET /sonarr/api/v3/system/status` (`settings:read`).
async fn system_status(
    _: RequireApiKey<SettingsRead>,
    State(state): State<AppState>,
) -> Result<Json<SystemResource>, CompatError> {
    let sqlite_version = db::sqlite_version(&state.db)
        .await
        .map_err(|err| failed("sqlite_version", err))?;
    let startup_path = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let app_data = state
        .logs_dir
        .parent()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut resource = map::system(&SystemView {
        start_time: state.started_at.clone(),
        startup_path,
        app_data,
        os_name: std::env::consts::OS.to_owned(),
        os_version: String::new(),
        sqlite_version,
    });
    if !state.base_path.is_empty() {
        resource.url_base = format!("{}{}", state.base_path, arr_compat::sonarr::URL_BASE);
    }
    Ok(Json(resource))
}

/// `GET /sonarr/api/v3/qualityprofile` (and `/qualityProfile`) (`profiles:read`).
async fn list_quality_profiles(
    _: RequireApiKey<ProfilesRead>,
    State(state): State<AppState>,
) -> Result<Json<Vec<QualityProfileResource>>, CompatError> {
    let models = db::profiles::list(&state.db)
        .await
        .map_err(|err| failed("quality_profiles", err))?;
    let mut out = Vec::with_capacity(models.len());
    for model in models {
        let items: Vec<Item> = serde_json::from_value(model.items)
            .map_err(|err| failed("quality_profile_items", err))?;
        let view = ProfileView {
            id: model.id,
            name: model.name,
            upgrade_allowed: model.upgrade_allowed,
            cutoff_quality_id: model.cutoff_quality_id,
            items,
            min_format_score: model.min_format_score,
            cutoff_format_score: model.cutoff_format_score,
            min_upgrade_format_score: model.min_upgrade_format_score,
        };
        out.push(map::quality_profile(&view));
    }
    Ok(Json(out))
}

fn describe_root_folder(id: i32, path: String, known: &[String]) -> RootFolderResource {
    let dir = Path::new(&path);
    let on_disk = list_subdir_names(dir);
    let accessible = on_disk.is_ok();
    let (free_space, total_space) = if accessible {
        (
            fs4::available_space(dir)
                .ok()
                .and_then(|bytes| i64::try_from(bytes).ok()),
            fs4::total_space(dir)
                .ok()
                .and_then(|bytes| i64::try_from(bytes).ok()),
        )
    } else {
        (None, None)
    };
    let unmapped = root_folder::unmapped(on_disk.unwrap_or_default(), known);
    map::root_folder(&RootFolderView {
        id,
        path,
        accessible,
        free_space,
        total_space,
        unmapped,
    })
}

fn list_subdir_names(path: &Path) -> std::io::Result<Vec<String>> {
    Ok(std::fs::read_dir(path)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect())
}

/// `GET /sonarr/api/v3/rootfolder` (`settings:read`).
async fn list_root_folders(
    caller: RequireApiKey<SettingsRead>,
    State(state): State<AppState>,
) -> Result<Json<Vec<RootFolderResource>>, CompatError> {
    let models = db::root_folders::list(&state.db)
        .await
        .map_err(|err| failed("root_folders", err))?;
    let mut folders = Vec::with_capacity(models.len());
    for model in models {
        if !caller.allows_in_library(Permission::SettingsRead, model.library_id) {
            continue;
        }
        let known = db::root_folders::movie_folder_names(&state.db, model.id)
            .await
            .map_err(|err| failed("root_folder_known_names", err))?;
        folders.push((model.id, model.path, known));
    }
    let described = tokio::task::spawn_blocking(move || {
        folders
            .into_iter()
            .map(|(id, path, known)| describe_root_folder(id, path, &known))
            .collect()
    })
    .await
    .map_err(|err| failed("root_folders_blocking", err))?;
    Ok(Json(described))
}

/// `GET /sonarr/api/v3/languageprofile` (`profiles:read`).
///
/// Stubbed as an empty list per docs/14-compatibility.md: Sonarr v4+ has no
/// language profiles and Seerr tolerates an empty list.
async fn list_language_profiles(
    _: RequireApiKey<ProfilesRead>,
) -> Json<Vec<LanguageProfileResource>> {
    Json(Vec::new())
}

/// `GET /sonarr/api/v3/tag` (`library:read`).
async fn list_tags(
    _: RequireApiKey<LibraryRead>,
    State(state): State<AppState>,
) -> Result<Json<Vec<TagResource>>, CompatError> {
    let tags = db::tags::list(&state.db)
        .await
        .map_err(|err| failed("tags", err))?;
    Ok(Json(
        tags.into_iter()
            .map(|tag| map::tag(tag.id, &tag.label))
            .collect(),
    ))
}

/// `POST /sonarr/api/v3/tag` (`library:edit`).
async fn create_tag(
    caller: RequireApiKey<LibraryEdit>,
    State(state): State<AppState>,
    ip: ClientIp,
    body: Result<Json<TagResource>, JsonRejection>,
) -> Result<(StatusCode, Json<TagResource>), CompatError> {
    let Json(body) = body.map_err(|err| CompatError::bad_request(err.body_text()))?;
    let label = parse_tag_label(&body)?;
    let created = db::tags::create(&state.db, label)
        .await
        .map_err(tag_error)?;
    let event = Event::new("tag.created")
        .target("tag", created.id)
        .data(json!({ "label": created.label }));
    record(&state, &Actor::of(&state, &caller).await, &ip, event).await;
    Ok((
        StatusCode::CREATED,
        Json(map::tag(created.id, &created.label)),
    ))
}

/// `PUT /sonarr/api/v3/tag/{id}` (`library:edit`).
async fn update_tag(
    caller: RequireApiKey<LibraryEdit>,
    State(state): State<AppState>,
    ip: ClientIp,
    id: Result<UrlPath<i32>, PathRejection>,
    body: Result<Json<TagResource>, JsonRejection>,
) -> Result<Json<TagResource>, CompatError> {
    let UrlPath(id) = id.map_err(|err| CompatError::bad_request(err.body_text()))?;
    let Json(body) = body.map_err(|err| CompatError::bad_request(err.body_text()))?;
    let label = parse_tag_label(&body)?;
    let before = db::tags::find(&state.db, id)
        .await
        .map_err(|err| failed("tags", err))?
        .ok_or_else(CompatError::not_found)?;
    let renamed = db::tags::rename(&state.db, id, label)
        .await
        .map_err(tag_error)?;
    let event = Event::new("tag.renamed")
        .target("tag", id)
        .data(json!({ "old": before.label, "new": renamed.label }));
    record(&state, &Actor::of(&state, &caller).await, &ip, event).await;
    Ok(Json(map::tag(renamed.id, &renamed.label)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_base_matches_arr_compat_url_base() {
        assert_eq!(API_BASE, format!("{}/api/v3", arr_compat::sonarr::URL_BASE));
    }

    #[test]
    fn every_sonarr_route_declares_a_permission() {
        assert_eq!(routes().verify(), Ok(()));
    }
}
