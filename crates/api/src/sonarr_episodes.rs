//! Sonarr v3 compatibility episode endpoints (`GET /episode`,
//! `PUT /episode/monitor`) (docs/14-compatibility.md, section D "Sonarr
//! endpoints (M8)").

use std::collections::{BTreeSet, HashMap};

use arr_compat::sonarr::dto::{EpisodeResource, MediaInfoResource, ReleaseType};
use arr_compat::sonarr::map::{
    self, EpisodeFileView, EpisodeMonitorRequest, EpisodeView, MapError, episode_resource,
    series_resource,
};
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use db::entities::shows::Model as StoredShow;
use db::entities::{episode_files, episodes};
use unseenarr_core::permission::Permission;

use crate::AppState;
use crate::access::{Caller, LibraryEdit, LibraryRead};
use crate::audit::{Actor, ClientIp, Event, record};
use crate::compat::{CompatError, RequireApiKey};
use crate::radarr_movies::query_param;

fn db_failed(error: impl std::fmt::Display) -> CompatError {
    tracing::error!(%error, "sonarr compat episodes");
    CompatError::internal()
}

fn map_error(error: MapError) -> CompatError {
    match error {
        MapError::Missing(field) => CompatError::validation(field, "must not be empty"),
        MapError::Invalid(field, msg) => CompatError::validation(field, msg),
    }
}

fn required_series_id(params: &HashMap<String, String>) -> Result<i32, CompatError> {
    let raw = query_param(params, "seriesId")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CompatError::validation("seriesId", "must be a positive integer"))?;
    match raw.parse::<i32>() {
        Ok(id) if id > 0 => Ok(id),
        _ => Err(CompatError::validation(
            "seriesId",
            "must be a positive integer",
        )),
    }
}

fn optional_int(params: &HashMap<String, String>, name: &str) -> Result<Option<i32>, CompatError> {
    match query_param(params, name).map(str::trim) {
        None | Some("") => Ok(None),
        Some(raw) => raw
            .parse::<i32>()
            .map(Some)
            .map_err(|_| CompatError::validation(name, "must be an integer")),
    }
}

fn flag(params: &HashMap<String, String>, name: &str) -> bool {
    query_param(params, name).is_some_and(|value| {
        let value = value.trim();
        value.eq_ignore_ascii_case("true") || value == "1"
    })
}

async fn ensure_library_access(
    state: &AppState,
    caller: &Caller,
    permission: Permission,
    root_folder_id: i32,
) -> Result<(), CompatError> {
    if caller.has_all_libraries(permission) {
        return Ok(());
    }
    let library_id = db::libraries::library_of_root_folder(&state.db, root_folder_id)
        .await
        .map_err(db_failed)?
        .ok_or_else(CompatError::not_found)?;
    if !caller.allows_in_library(permission, library_id) {
        return Err(CompatError::not_found());
    }
    Ok(())
}

/// The folder Sonarr shows as the file's parent: the root folder joined with
/// the show folder.
fn show_path(root_path: &str, show: &StoredShow) -> String {
    if root_path.is_empty() || show.folder_name.is_empty() {
        String::new()
    } else {
        map::join(root_path, &show.folder_name)
    }
}

fn episode_view(
    episode: &episodes::Model,
    files: &HashMap<i32, episode_files::Model>,
    file_of_episode: &HashMap<i32, i32>,
) -> EpisodeView {
    let file = file_of_episode
        .get(&episode.id)
        .and_then(|file_id| files.get(file_id))
        .map(|file| EpisodeFileView {
            id: file.id,
            series_id: file.show_id,
            season_number: episode.season_number,
            relative_path: file.relative_path.clone(),
            size: file.size,
            quality_id: file.quality_id,
            release_group: file.release_group.clone(),
            scene_name: file.scene_name.clone(),
            languages: file
                .languages
                .clone()
                .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
                .unwrap_or_default(),
            release_type: ReleaseType::SingleEpisode,
            media_info: file
                .media_info
                .clone()
                .and_then(|value| serde_json::from_value::<MediaInfoResource>(value).ok()),
            quality_cutoff_not_met: false,
            added_at: file.added_at.clone(),
        });
    EpisodeView {
        id: episode.id,
        series_id: episode.show_id,
        tvdb_id: episode.tvdb_id,
        season_number: episode.season_number,
        episode_number: episode.episode_number,
        absolute_number: episode.absolute_number,
        title: episode.title.clone(),
        overview: episode.overview.clone(),
        air_date: episode.air_date.clone(),
        air_date_utc: episode.air_date_utc.clone(),
        runtime: episode.runtime,
        monitored: episode.monitored,
        file,
    }
}

/// `GET /sonarr/api/v3/episode?seriesId={seriesId}` (`library:read`).
///
/// `seasonNumber` narrows the list; `includeEpisodeFile` and `includeSeries`
/// default to false like Sonarr, which Seerr relies on for the light list.
pub async fn list_episodes(
    caller: RequireApiKey<LibraryRead>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<EpisodeResource>>, CompatError> {
    let show_id = required_series_id(&params)?;
    let season_number = optional_int(&params, "seasonNumber")?;
    let include_episode_file = flag(&params, "includeEpisodeFile");
    let include_series = flag(&params, "includeSeries");
    let show = db::shows::find(&state.db, show_id)
        .await
        .map_err(db_failed)?
        .ok_or_else(CompatError::not_found)?;
    ensure_library_access(
        &state,
        &caller,
        Permission::LibraryRead,
        show.root_folder_id,
    )
    .await?;
    let root_path = db::root_folders::find(&state.db, show.root_folder_id)
        .await
        .map_err(db_failed)?
        .map(|folder| folder.path)
        .unwrap_or_default();
    let (episodes, files, links) = tokio::try_join!(
        async {
            match season_number {
                Some(season_number) => {
                    db::shows::episodes_of_season(&state.db, show_id, season_number).await
                }
                None => db::shows::episodes_of(&state.db, show_id).await,
            }
        },
        db::shows::files_of(&state.db, show_id),
        async {
            let show_ids = [show_id];
            db::shows::file_links_of(&state.db, &show_ids).await
        },
    )
    .map_err(db_failed)?;
    let series = if include_series {
        crate::sonarr_series::load_series_views(&state, std::slice::from_ref(&show))
            .await?
            .into_iter()
            .next()
            .map(|view| series_resource(&view))
    } else {
        None
    };
    let files: HashMap<i32, episode_files::Model> =
        files.into_iter().map(|file| (file.id, file)).collect();
    let file_of_episode: HashMap<i32, i32> = links
        .into_iter()
        .map(|link| (link.episode_id, link.episode_file_id))
        .collect();
    let path = show_path(&root_path, &show);
    let resources = episodes
        .iter()
        .map(|episode| {
            let view = episode_view(episode, &files, &file_of_episode);
            let mut resource = episode_resource(&view, &path);
            if !include_episode_file {
                resource.episode_file = None;
            }
            resource.series.clone_from(&series);
            resource
        })
        .collect();
    Ok(Json(resources))
}

/// `PUT /sonarr/api/v3/episode/monitor` (`library:edit`).
pub async fn monitor_episodes(
    caller: RequireApiKey<LibraryEdit>,
    State(state): State<AppState>,
    ip: ClientIp,
    body: Result<Json<EpisodeMonitorRequest>, JsonRejection>,
) -> Result<StatusCode, CompatError> {
    let Json(body) = body.map_err(|err| CompatError::bad_request(err.body_text()))?;
    let monitor = body.into_monitor().map_err(map_error)?;
    let mut ids = monitor.episode_ids;
    ids.sort_unstable();
    ids.dedup();
    let found = db::shows::episodes_by_ids(&state.db, &ids)
        .await
        .map_err(db_failed)?;
    if found.len() != ids.len() {
        return Err(CompatError::not_found());
    }
    let show_ids: BTreeSet<i32> = found.iter().map(|episode| episode.show_id).collect();
    for show_id in show_ids {
        let show = db::shows::find(&state.db, show_id)
            .await
            .map_err(db_failed)?
            .ok_or_else(CompatError::not_found)?;
        ensure_library_access(
            &state,
            &caller,
            Permission::LibraryEdit,
            show.root_folder_id,
        )
        .await?;
    }
    let actor = Actor::of(&state, &caller).await;
    db::shows::set_episodes_monitored(&state.db, &ids, monitor.monitored)
        .await
        .map_err(db_failed)?;
    for id in ids {
        let event = Event::new("episode.monitored")
            .target("episode", id)
            .data(serde_json::json!({ "monitored": monitor.monitored }));
        record(&state, &actor, &ip, event).await;
    }
    Ok(StatusCode::OK)
}
