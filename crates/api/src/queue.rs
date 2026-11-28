//! Activity queue (`GET /api/v1/queue`, `DELETE /api/v1/queue/{id}`).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use db::entities::download_clients;
use serde::{Deserialize, Serialize};
use serde_json::json;
use unseenarr_core::permission::Permission;
use unseenarr_core::qbittorrent::Torrent;
use unseenarr_core::sabnzbd::QueueItem as SabSlot;
use utoipa::{IntoParams, ToSchema};

use crate::AppState;
use crate::access::{
    QueueManage, QueueRead, Require, allowed_root_folders, ensure_root_folder_access,
};
use crate::audit::{Actor, ClientIp, Event, record};
use crate::download_clients::{ConnectedClient, connect, qb_error, sab_error};
use crate::error::Problem;

/// qBittorrent reports `8640000` (100 days) when the ETA is unknown.
const QB_UNKNOWN_ETA: i64 = 8_640_000;

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub id: i32,
    pub movie_id: i32,
    pub movie_title: String,
    pub movie_year: i32,
    pub movie_profile_id: Option<i32>,
    pub download_client_id: Option<i32>,
    pub download_client_name: Option<String>,
    pub release_title: String,
    pub quality_id: i32,
    pub custom_format_score: i32,
    pub size: i64,
    /// Progress from `0.0` to `1.0`.
    pub progress: f64,
    /// Download speed in bytes per second.
    pub speed: i64,
    /// Estimated seconds remaining, or null when unknown.
    pub eta: Option<i64>,
    /// `queued`, `downloading`, `paused`, `completed`, `importing`, `importFailed`.
    pub state: String,
    pub error: Option<String>,
    pub grabbed_at: String,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct RemoveQueueQuery {
    /// Also delete the item and its files from the download client (default true).
    #[serde(default = "default_true")]
    pub remove_from_client: bool,
    /// Add the release to the movie's blocklist so it is not grabbed again (default false).
    #[serde(default)]
    pub blocklist: bool,
}

pub(crate) struct LiveStatus {
    pub(crate) progress: f64,
    pub(crate) speed: i64,
    pub(crate) eta: Option<i64>,
    pub(crate) state: Option<&'static str>,
}

fn failed(error: impl std::fmt::Display) -> Problem {
    tracing::error!(%error, "queue");
    Problem::internal()
}

fn map_qb_state(raw: &str) -> Option<&'static str> {
    match raw {
        "pausedDL" | "stoppedDL" => Some("paused"),
        "queuedDL" | "allocating" | "checkingDL" | "checkingResumeData" | "metaDL"
        | "forcedMetaDL" => Some("queued"),
        "downloading" | "forcedDL" | "stalledDL" => Some("downloading"),
        "uploading" | "stalledUP" | "pausedUP" | "stoppedUP" | "queuedUP" | "forcedUP"
        | "checkingUP" => Some("completed"),
        _ => None,
    }
}

fn map_sab_state(raw: &str) -> Option<&'static str> {
    match raw {
        "Paused" => Some("paused"),
        "Queued" => Some("queued"),
        "Downloading" | "Fetching" | "Grabbing" => Some("downloading"),
        _ => None,
    }
}

pub(crate) fn status_from_torrent(torrent: &Torrent) -> LiveStatus {
    let eta = (torrent.eta >= 0 && torrent.eta < QB_UNKNOWN_ETA).then_some(torrent.eta);
    LiveStatus {
        progress: torrent.progress.clamp(0.0, 1.0),
        speed: torrent.dlspeed.max(0),
        eta,
        state: map_qb_state(&torrent.state),
    }
}

pub(crate) fn status_from_sab(slot: &SabSlot) -> LiveStatus {
    let pct = slot.percentage.trim().parse::<f64>().unwrap_or(0.0);
    LiveStatus {
        progress: (pct / 100.0).clamp(0.0, 1.0),
        speed: 0,
        eta: None,
        state: map_sab_state(&slot.status),
    }
}

async fn fetch_client_statuses(
    state: &AppState,
    client: &download_clients::Model,
) -> HashMap<String, LiveStatus> {
    let connected = match connect(state, client) {
        Ok(connected) => connected,
        Err(problem) => {
            tracing::warn!(
                client_id = client.id,
                code = %problem.code,
                "queue: unable to connect to download client"
            );
            return HashMap::new();
        }
    };

    match connected {
        ConnectedClient::Qbittorrent(qb) => match qb.torrents(&client.category).await {
            Ok(torrents) => torrents
                .into_iter()
                .map(|torrent| {
                    let status = status_from_torrent(&torrent);
                    (torrent.hash, status)
                })
                .collect(),
            Err(error) => {
                tracing::warn!(client_id = client.id, %error, "queue: qBittorrent poll failed");
                HashMap::new()
            }
        },
        ConnectedClient::Sabnzbd(sab) => match sab.queue(&client.category).await {
            Ok(slots) => slots
                .into_iter()
                .map(|slot| {
                    let status = status_from_sab(&slot);
                    (slot.nzo_id, status)
                })
                .collect(),
            Err(error) => {
                tracing::warn!(client_id = client.id, %error, "queue: SABnzbd poll failed");
                HashMap::new()
            }
        },
    }
}

/// Active downloads in the queue, enriched with live progress from each client.
#[utoipa::path(
    get,
    path = "/api/v1/queue",
    tag = "queue",
    responses(
        (status = 200, description = "Active downloads", body = Vec<QueueItem>),
        (status = 403, description = "Permission denied", body = Problem),
    )
)]
pub async fn list_queue(
    caller: Require<QueueRead>,
    State(state): State<AppState>,
) -> Result<Json<Vec<QueueItem>>, Problem> {
    let allowed = allowed_root_folders(&state, &caller, Permission::QueueRead).await?;
    let rows = db::downloads::list_active(&state.db)
        .await
        .map_err(failed)?;
    let rows: Vec<_> = match allowed {
        None => rows,
        Some(allowed) => rows
            .into_iter()
            .filter(|row| allowed.contains(&row.movie_root_folder_id))
            .collect(),
    };

    // Fetch live status once per distinct download client referenced by active rows.
    let mut clients: HashMap<i32, download_clients::Model> = HashMap::new();
    for row in &rows {
        if let Some(client) = &row.client {
            clients.entry(client.id).or_insert_with(|| client.clone());
        }
    }

    let mut live_by_client: HashMap<i32, HashMap<String, LiveStatus>> = HashMap::new();
    for (client_id, model) in clients {
        let statuses = fetch_client_statuses(&state, &model).await;
        live_by_client.insert(client_id, statuses);
    }

    let items = rows
        .into_iter()
        .map(|row| {
            let dl = row.download;
            let live = dl.download_client_id.and_then(|client_id| {
                let key = if row.client.as_ref().is_some_and(|c| c.kind == "qbittorrent") {
                    dl.client_download_id.to_ascii_lowercase()
                } else {
                    dl.client_download_id.clone()
                };
                live_by_client.get(&client_id)?.get(&key)
            });

            let default_progress = if matches!(dl.state.as_str(), "completed" | "importing") {
                1.0
            } else {
                0.0
            };

            let (progress, speed, eta, effective_state) = match live {
                Some(status) => {
                    let effective_state =
                        if matches!(dl.state.as_str(), "importing" | "importFailed") {
                            dl.state.clone()
                        } else {
                            status
                                .state
                                .map(str::to_owned)
                                .unwrap_or_else(|| dl.state.clone())
                        };
                    (status.progress, status.speed, status.eta, effective_state)
                }
                None => (default_progress, 0, None, dl.state),
            };

            QueueItem {
                id: dl.id,
                movie_id: row.movie_id,
                movie_title: row.movie_title,
                movie_year: row.movie_year,
                movie_profile_id: dl.movie_profile_id,
                download_client_id: dl.download_client_id,
                download_client_name: row.client.map(|c| c.name),
                release_title: dl.release_title,
                quality_id: dl.quality_id,
                custom_format_score: dl.custom_format_score,
                size: dl.size,
                progress,
                speed,
                eta,
                state: effective_state,
                error: dl.error,
                grabbed_at: dl.grabbed_at,
            }
        })
        .collect();

    Ok(Json(items))
}

/// Removes an active download from the queue, optionally deleting it from the
/// download client and/or adding its release to the blocklist.
#[utoipa::path(
    delete,
    path = "/api/v1/queue/{id}",
    tag = "queue",
    params(
        ("id" = i32, Path, description = "Download id"),
        RemoveQueueQuery,
    ),
    responses(
        (status = 204, description = "Removed from queue"),
        (status = 403, description = "Permission denied", body = Problem),
        (status = 404, description = "No such active download", body = Problem),
        (status = 409, description = "The stored client credentials cannot be read", body = Problem),
        (status = 502, description = "The download client refused or failed the delete", body = Problem),
    )
)]
pub async fn remove_queue_item(
    caller: Require<QueueManage>,
    State(state): State<AppState>,
    ip: ClientIp,
    Path(id): Path<i32>,
    Query(query): Query<RemoveQueueQuery>,
) -> Result<StatusCode, Problem> {
    let existing = db::downloads::find(&state.db, id)
        .await
        .map_err(failed)?
        .ok_or_else(Problem::not_found)?;
    if db::downloads::is_final_state(&existing.state) {
        return Err(Problem::not_found());
    }
    // TODO(#91): remove TV downloads too.
    let Some(movie_id) = existing.movie_id else {
        return Err(Problem::not_found());
    };
    let movie = db::movies::find(&state.db, movie_id)
        .await
        .map_err(failed)?
        .ok_or_else(Problem::not_found)?;
    let library_id = ensure_root_folder_access(
        &state,
        &caller,
        Permission::QueueManage,
        movie.model.root_folder_id,
    )
    .await?;

    if query.remove_from_client
        && let Some(client_id) = existing.download_client_id
        && let Some(stored_client) = db::download_clients::find(&state.db, client_id)
            .await
            .map_err(failed)?
    {
        let model = stored_client.model;
        let ids = [existing.client_download_id.clone()];
        match connect(&state, &model)? {
            ConnectedClient::Qbittorrent(qb) => {
                qb.delete(&model.category, &ids, true)
                    .await
                    .map_err(qb_error)?;
            }
            ConnectedClient::Sabnzbd(sab) => {
                sab.delete(&model.category, &ids, true)
                    .await
                    .map_err(sab_error)?;
            }
        }
    }

    let previous =
        db::downloads::mark_removed(&state.db, id, query.blocklist, "Removed from queue")
            .await
            .map_err(failed)?
            .ok_or_else(Problem::not_found)?;

    let event = Event::new("queue.removed")
        .target("download", id)
        .data(json!({
            "releaseTitle": previous.release_title,
            "removeFromClient": query.remove_from_client,
            "blocklist": query.blocklist,
        }));
    record(&state, &Actor::of(&state, &caller).await, &ip, event).await;

    state.publish_download_state_changed_in_library(id, &previous.state, "removed", library_id);

    Ok(StatusCode::NO_CONTENT)
}
