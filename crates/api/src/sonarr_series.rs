//! Sonarr v3 compatibility series read endpoints (`GET /series`,
//! `GET /series?tvdbId=`, `GET /series/{id}`) (docs/14-compatibility.md,
//! section D "Sonarr endpoints (M8)").
//!
//! `GET /series/lookup` and the add, update and delete endpoints need the TVDB
//! metadata lookup (#87) and the shows CRUD layer (#88), so they report the
//! gap like the router's unimplemented fallback.

use std::collections::{BTreeSet, HashMap};

use arr_compat::sonarr::dto::SeriesResource;
use arr_compat::sonarr::map::{SeasonView, SeriesView, series_resource};
use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use db::entities::episode_file_episodes;
use db::entities::episode_files;
use db::entities::episodes;
use db::entities::seasons;
use db::entities::shows::Model as StoredShow;
use unseenarr_core::permission::Permission;

use crate::AppState;
use crate::compat::{ApiKeyCaller, CompatError, OriginalCompatPath};
use crate::radarr_movies::{allowed_folders_for_read, query_param};

fn db_failed(error: impl std::fmt::Display) -> CompatError {
    tracing::error!(%error, "sonarr compat series");
    CompatError::internal()
}

/// The first 19 characters of a timestamp (`2026-11-24T10:00:00`), enough to
/// order ISO 8601 dates lexically.
fn air_key(value: &str) -> &str {
    value
        .char_indices()
        .nth(19)
        .map_or(value, |(at, _)| &value[..at])
}

fn aired(episode: &episodes::Model) -> Option<&str> {
    episode
        .air_date_utc
        .as_deref()
        .or(episode.air_date.as_deref())
}

#[derive(Debug, Default)]
struct Airing {
    previous: Option<String>,
    next: Option<String>,
}

fn airing<'a>(dates: impl Iterator<Item = &'a str>, now: &str) -> Airing {
    let now_key = air_key(now);
    let mut previous: Option<(&str, &str)> = None;
    let mut next: Option<(&str, &str)> = None;
    for date in dates {
        let key = air_key(date);
        if key > now_key {
            if next.is_none_or(|(best, _)| key < best) {
                next = Some((key, date));
            }
        } else if previous.is_none_or(|(best, _)| key > best) {
            previous = Some((key, date));
        }
    }
    Airing {
        previous: previous.map(|(_, date)| date.to_owned()),
        next: next.map(|(_, date)| date.to_owned()),
    }
}

fn season_view(
    season: &seasons::Model,
    season_episodes: &[&episodes::Model],
    files: &HashMap<i32, &episode_files::Model>,
    file_of_episode: &HashMap<i32, i32>,
    now: &str,
) -> SeasonView {
    let mut file_ids = BTreeSet::new();
    let mut episode_file_count = 0;
    for episode in season_episodes {
        if let Some(file_id) = file_of_episode.get(&episode.id) {
            episode_file_count += 1;
            file_ids.insert(*file_id);
        }
    }
    let mut size_on_disk = 0;
    let mut release_groups = Vec::new();
    for file_id in file_ids {
        if let Some(file) = files.get(&file_id) {
            size_on_disk += file.size;
            if let Some(group) = file.release_group.clone()
                && !release_groups.contains(&group)
            {
                release_groups.push(group);
            }
        }
    }
    let airing = airing(season_episodes.iter().filter_map(|e| aired(e)), now);
    SeasonView {
        season_number: season.season_number,
        monitored: season.monitored,
        title: None,
        next_airing: airing.next,
        previous_airing: airing.previous,
        episode_file_count,
        episode_count: i32::try_from(season_episodes.iter().filter(|e| e.monitored).count())
            .unwrap_or(i32::MAX),
        total_episode_count: i32::try_from(season_episodes.len()).unwrap_or(i32::MAX),
        size_on_disk,
        release_groups,
    }
}

fn build_series_view(
    show: &StoredShow,
    root_folder_path: String,
    seasons: Vec<seasons::Model>,
    episodes: Vec<episodes::Model>,
    files: Vec<episode_files::Model>,
    links: Vec<episode_file_episodes::Model>,
    now: &str,
) -> SeriesView {
    let files: HashMap<i32, &episode_files::Model> =
        files.iter().map(|file| (file.id, file)).collect();
    let file_of_episode: HashMap<i32, i32> = links
        .iter()
        .map(|link| (link.episode_id, link.episode_file_id))
        .collect();
    let mut by_season: HashMap<i32, Vec<&episodes::Model>> = HashMap::new();
    for episode in &episodes {
        by_season
            .entry(episode.season_number)
            .or_default()
            .push(episode);
    }
    let season_views = seasons
        .iter()
        .map(|season| {
            let season_episodes = by_season
                .get(&season.season_number)
                .map_or(&[][..], Vec::as_slice);
            season_view(season, season_episodes, &files, &file_of_episode, now)
        })
        .collect();
    let all = airing(episodes.iter().filter_map(aired), now);
    let first_aired = episodes
        .iter()
        .filter_map(aired)
        .min_by_key(|date| air_key(date))
        .map(str::to_owned);
    SeriesView {
        id: show.id,
        tvdb_id: show.tvdb_id,
        tmdb_id: show.tmdb_id,
        imdb_id: show.imdb_id.clone(),
        title: show.title.clone(),
        original_title: show.original_title.clone(),
        sort_title: show.sort_title.clone(),
        year: show.year,
        overview: show.overview.clone(),
        runtime: show.runtime,
        status: show.status.clone(),
        series_type: show.series_type.clone(),
        network: show.network.clone(),
        poster_path: show.poster_path.clone(),
        season_folder: show.season_folder,
        root_folder_path,
        folder_name: show.folder_name.clone(),
        monitored: show.monitored,
        monitor_new_items: show.monitor_new_items.clone(),
        first_aired,
        last_aired: all.previous.clone(),
        next_airing: all.next,
        previous_airing: all.previous,
        original_language: None,
        added_at: show.added_at.clone(),
        aliases: Vec::new(),
        tags: Vec::new(),
        quality_profile_id: 0,
        seasons: season_views,
    }
}

fn group_by_show<T>(items: Vec<T>, show_id: impl Fn(&T) -> i32) -> HashMap<i32, Vec<T>> {
    let mut grouped: HashMap<i32, Vec<T>> = HashMap::new();
    for item in items {
        grouped.entry(show_id(&item)).or_default().push(item);
    }
    grouped
}

pub(crate) async fn load_series_views(
    state: &AppState,
    shows: &[StoredShow],
) -> Result<Vec<SeriesView>, CompatError> {
    if shows.is_empty() {
        return Ok(Vec::new());
    }
    let folders = db::root_folders::list(&state.db).await.map_err(db_failed)?;
    let paths: HashMap<i32, String> = folders
        .into_iter()
        .map(|folder| (folder.id, folder.path))
        .collect();
    let now = db::now();
    let show_ids: Vec<i32> = shows.iter().map(|show| show.id).collect();
    let (seasons, episodes, files, links) = tokio::try_join!(
        db::shows::seasons_of_shows(&state.db, &show_ids),
        db::shows::episodes_of_shows(&state.db, &show_ids),
        db::shows::files_of_shows(&state.db, &show_ids),
        db::shows::file_links_of(&state.db, &show_ids),
    )
    .map_err(db_failed)?;
    let show_of_file: HashMap<i32, i32> =
        files.iter().map(|file| (file.id, file.show_id)).collect();
    let mut seasons_by_show = group_by_show(seasons, |season| season.show_id);
    let mut episodes_by_show = group_by_show(episodes, |episode| episode.show_id);
    let mut files_by_show = group_by_show(files, |file| file.show_id);
    let mut links_by_show: HashMap<i32, Vec<episode_file_episodes::Model>> = HashMap::new();
    for link in links {
        if let Some(show_id) = show_of_file.get(&link.episode_file_id) {
            links_by_show.entry(*show_id).or_default().push(link);
        }
    }
    let mut views = Vec::with_capacity(shows.len());
    for show in shows {
        views.push(build_series_view(
            show,
            paths.get(&show.root_folder_id).cloned().unwrap_or_default(),
            seasons_by_show.remove(&show.id).unwrap_or_default(),
            episodes_by_show.remove(&show.id).unwrap_or_default(),
            files_by_show.remove(&show.id).unwrap_or_default(),
            links_by_show.remove(&show.id).unwrap_or_default(),
            &now,
        ));
    }
    Ok(views)
}

/// `GET /sonarr/api/v3/series` and `GET /sonarr/api/v3/series?tvdbId={tvdbId}`
/// (`library:read`).
pub async fn list_series(
    caller: ApiKeyCaller,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<SeriesResource>>, CompatError> {
    let allowed = allowed_folders_for_read(&state, &caller).await?;
    let models = match query_param(&params, "tvdbId").map(str::trim) {
        Some(raw) if !raw.is_empty() => {
            let tvdb_id = raw
                .parse::<i32>()
                .map_err(|_| CompatError::validation("tvdbId", "must be an integer"))?;
            if tvdb_id <= 0 {
                return Ok(Json(Vec::new()));
            }
            db::shows::find_by_tvdb_id(&state.db, tvdb_id)
                .await
                .map_err(db_failed)?
                .into_iter()
                .collect()
        }
        _ => db::shows::list(&state.db).await.map_err(db_failed)?,
    };
    let models: Vec<_> = match allowed {
        None => models,
        Some(allowed) => models
            .into_iter()
            .filter(|show| allowed.contains(&show.root_folder_id))
            .collect(),
    };
    let views = load_series_views(&state, &models).await?;
    Ok(Json(views.iter().map(series_resource).collect()))
}

/// `GET /sonarr/api/v3/series/{id}` (`library:read`).
pub async fn get_series(
    caller: ApiKeyCaller,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> Result<Json<SeriesResource>, CompatError> {
    let id = raw_id
        .parse::<i32>()
        .map_err(|_| CompatError::bad_request("invalid series id"))?;
    if id <= 0 {
        return Err(CompatError::not_found());
    }
    let show = db::shows::find(&state.db, id)
        .await
        .map_err(db_failed)?
        .ok_or_else(CompatError::not_found)?;
    if !caller.has_all_libraries(Permission::LibraryRead) {
        let library_id = db::libraries::library_of_root_folder(&state.db, show.root_folder_id)
            .await
            .map_err(db_failed)?
            .ok_or_else(CompatError::not_found)?;
        if !caller.allows_in_library(Permission::LibraryRead, library_id) {
            return Err(CompatError::not_found());
        }
    }
    let views = load_series_views(&state, std::slice::from_ref(&show)).await?;
    let view = views.first().ok_or_else(CompatError::not_found)?;
    Ok(Json(series_resource(view)))
}

/// `GET /sonarr/api/v3/series/lookup` (`library:read`).
///
/// Seerr calls this before every add; it needs the TVDB lookup from #87 and
/// the shows CRUD layer from #88, so it reports the gap like the router's
/// unimplemented fallback instead of matching `series/{id}`.
pub async fn lookup_series(original: Option<Extension<OriginalCompatPath>>) -> Response {
    let path = original.map_or_else(
        || "/sonarr/api/v3/series/lookup".to_owned(),
        |Extension(OriginalCompatPath(path))| path,
    );
    tracing::warn!(method = "GET", path = %path, "unimplemented compat endpoint");
    CompatError::not_implemented(&Method::GET, path).into_response()
}
