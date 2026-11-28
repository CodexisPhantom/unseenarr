//! Download polling (docs/06-state-machines.md, "Download" and "Retries").
//!
//! Reconciles the active downloads with their clients: on startup once, then
//! in a loop while downloads are active. The client is the source of truth
//! only before an import starts; the state machine in
//! [`unseenarr_core::download`] decides every move.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use better_auth::seaorm::sea_orm::DbErr;
use db::entities::download_clients;
use unseenarr_core::download::{DownloadState, Observation, PollSettings, Tracking, poll};

use crate::AppState;
use crate::download_clients::{ConnectedClient, connect};
use crate::queue::{status_from_sab, status_from_torrent};

/// Database setting for the stall limit, in minutes.
const STALLED_KEY: &str = "downloads.stalled_timeout_minutes";
/// Default stall limit, in minutes (docs/06-state-machines.md).
const DEFAULT_STALLED_MINUTES: i64 = 360;

/// One poll of one client per active download, keyed by download id.
pub struct Poller {
    tracking: HashMap<i32, Tracking>,
}

impl Poller {
    /// A poller that has not seen any download yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tracking: HashMap::new(),
        }
    }

    /// Polls each enabled client once. `fail_after_missing_polls` is 1 at
    /// startup and 2 in the loop.
    pub async fn poll(
        &mut self,
        state: &AppState,
        now: i64,
        fail_after_missing_polls: u32,
    ) -> Result<Summary, DbErr> {
        let settings = PollSettings {
            stalled_after_seconds: stalled_timeout_seconds(state).await,
            fail_after_missing_polls: fail_after_missing_polls.max(1),
        };
        let rows = db::downloads::list_active(&state.db).await?;
        let mut kept: Vec<(db::downloads::QueuedDownload, DownloadState)> = Vec::new();
        for row in rows {
            if let Some(current) = DownloadState::parse(&row.download.state)
                && current.is_polled()
            {
                kept.push((row, current));
            }
        }
        let active = !kept.is_empty();
        // One query per poll maps each movie folder to its library for event
        // scoping.
        let folder_library: HashMap<i32, i32> = db::root_folders::list(&state.db)
            .await?
            .into_iter()
            .map(|folder| (folder.id, folder.library_id))
            .collect();

        let mut by_client: HashMap<i32, (download_clients::Model, Vec<(usize, String)>)> =
            HashMap::new();
        for (index, (row, _)) in kept.iter().enumerate() {
            let Some(client_id) = row.download.download_client_id else {
                continue;
            };
            let Some(client) = row.client.clone() else {
                continue;
            };
            if !client.enabled {
                continue;
            }
            let key = lookup_key(&client.kind, &row.download.client_download_id);
            by_client
                .entry(client_id)
                .or_insert_with(|| (client, Vec::new()))
                .1
                .push((index, key));
        }

        let mut changed = 0;
        let mut failed = 0;
        for (client, items) in by_client.values() {
            let keys: HashSet<String> = items.iter().map(|(_, key)| key.clone()).collect();
            let observed = observe(state, client, &keys).await;
            for (index, key) in items {
                let (row, current) = &kept[*index];
                let observation: Observation = match &observed {
                    None => Observation::Unreachable,
                    Some(map) => map.get(key).cloned().unwrap_or(Observation::Missing),
                };
                let seen_progress = match &observation {
                    Observation::Present { progress, .. } => *progress,
                    Observation::Missing | Observation::Unreachable => 0.0,
                };
                let tracking = *self
                    .tracking
                    .entry(row.download.id)
                    .or_insert_with(|| Tracking::new(seen_progress, now));
                let decision = poll(*current, &tracking, &observation, settings, now);
                self.tracking.insert(row.download.id, decision.tracking);
                let library = folder_library.get(&row.movie_root_folder_id).copied();
                if decision.state != *current {
                    let Some((before, after)) = db::downloads::advance(
                        &state.db,
                        row.download.id,
                        decision.state,
                        decision.error.clone(),
                    )
                    .await?
                    else {
                        continue;
                    };
                    changed += 1;
                    if decision.state == DownloadState::Failed {
                        failed += 1;
                    }
                    match library {
                        Some(library_id) => {
                            state.publish_download_state_changed_in_library(
                                row.download.id,
                                &before.state,
                                &after.state,
                                library_id,
                            );
                            state.publish_download_progress_update_in_library(
                                row.download.id,
                                decision.progress,
                                decision.speed,
                                decision.eta,
                                &after.state,
                                library_id,
                            );
                        }
                        None => {
                            state.publish_download_state_changed(
                                row.download.id,
                                &before.state,
                                &after.state,
                            );
                            state.publish_download_progress_update(
                                row.download.id,
                                decision.progress,
                                decision.speed,
                                decision.eta,
                                &after.state,
                            );
                        }
                    }
                } else if matches!(observation, Observation::Present { .. }) {
                    match library {
                        Some(library_id) => {
                            state.publish_download_progress_update_in_library(
                                row.download.id,
                                decision.progress,
                                decision.speed,
                                decision.eta,
                                &row.download.state,
                                library_id,
                            );
                        }
                        None => {
                            state.publish_download_progress_update(
                                row.download.id,
                                decision.progress,
                                decision.speed,
                                decision.eta,
                                &row.download.state,
                            );
                        }
                    }
                }
            }
        }

        let seen: HashSet<i32> = kept.iter().map(|(row, _)| row.download.id).collect();
        self.tracking.retain(|id, _| seen.contains(id));
        Ok(Summary {
            active,
            changed,
            failed,
        })
    }
}

impl Default for Poller {
    fn default() -> Self {
        Self::new()
    }
}

/// What one [`Poller::poll`] did.
pub struct Summary {
    pub active: bool,
    pub changed: usize,
    pub failed: usize,
}

/// The key a row is looked up with: qBittorrent hashes are lower case.
fn lookup_key(kind: &str, client_download_id: &str) -> String {
    if kind == "qbittorrent" {
        client_download_id.to_ascii_lowercase()
    } else {
        client_download_id.to_owned()
    }
}

/// One fetch of what `client` says about the wanted `keys`. `None` means the
/// client did not answer; a `Some` map without a key means the item is
/// missing from the client.
async fn observe(
    state: &AppState,
    client: &download_clients::Model,
    keys: &HashSet<String>,
) -> Option<HashMap<String, Observation>> {
    let connected = match connect(state, client) {
        Ok(connected) => connected,
        Err(_) => return None,
    };
    match connected {
        ConnectedClient::Qbittorrent(qb) => match qb.torrents(&client.category).await {
            Err(_) => None,
            Ok(torrents) => Some(
                torrents
                    .into_iter()
                    .map(|torrent| {
                        let live = status_from_torrent(&torrent);
                        let observation = match live.state {
                            Some("paused") => Observation::Present {
                                progress: live.progress,
                                queued: false,
                                paused: true,
                                complete: false,
                                speed: 0,
                                eta: live.eta,
                                error: None,
                            },
                            Some("completed") => Observation::Present {
                                progress: 1.0,
                                queued: false,
                                paused: false,
                                complete: true,
                                speed: 0,
                                eta: None,
                                error: None,
                            },
                            Some("queued") => Observation::Present {
                                progress: live.progress,
                                queued: true,
                                paused: false,
                                complete: false,
                                speed: 0,
                                eta: live.eta,
                                error: None,
                            },
                            Some("downloading") => Observation::Present {
                                progress: live.progress,
                                queued: false,
                                paused: false,
                                complete: false,
                                speed: live.speed,
                                eta: live.eta,
                                error: None,
                            },
                            _ => Observation::Present {
                                progress: live.progress,
                                queued: false,
                                paused: false,
                                complete: false,
                                speed: live.speed,
                                eta: live.eta,
                                error: matches!(torrent.state.as_str(), "error" | "missingFiles")
                                    .then(|| {
                                        format!(
                                            "qBittorrent reports the torrent in state {}",
                                            torrent.state
                                        )
                                    }),
                            },
                        };
                        (torrent.hash.to_ascii_lowercase(), observation)
                    })
                    .collect(),
            ),
        },
        ConnectedClient::Sabnzbd(sab) => {
            let slots = match sab.queue(&client.category).await {
                Err(_) => return None,
                Ok(slots) => slots,
            };
            let mut map: HashMap<String, Observation> = slots
                .into_iter()
                .map(|slot| {
                    let live = status_from_sab(&slot);
                    let observation = match live.state {
                        Some("paused") => Observation::Present {
                            progress: live.progress,
                            queued: false,
                            paused: true,
                            complete: false,
                            speed: 0,
                            eta: None,
                            error: None,
                        },
                        _ => Observation::Present {
                            progress: live.progress,
                            queued: live.state == Some("queued"),
                            paused: false,
                            complete: false,
                            speed: 0,
                            eta: None,
                            error: None,
                        },
                    };
                    (slot.nzo_id, observation)
                })
                .collect();
            if keys.iter().any(|key| !map.contains_key(key)) {
                // A history that cannot be read is not proof the job is gone.
                let history = match sab.history(&client.category).await {
                    Ok(history) => history,
                    Err(_) => return None,
                };
                let by_id: HashMap<&str, _> = history
                    .iter()
                    .map(|item| (item.nzo_id.as_str(), item))
                    .collect();
                for key in keys {
                    if map.contains_key(key) {
                        continue;
                    }
                    let Some(item) = by_id.get(key.as_str()) else {
                        continue;
                    };
                    let observation = match item.status.as_str() {
                        "Completed" => Observation::Present {
                            progress: 1.0,
                            queued: false,
                            paused: false,
                            complete: true,
                            speed: 0,
                            eta: None,
                            error: None,
                        },
                        "Failed" => {
                            let reason = item.fail_message.trim();
                            Observation::Present {
                                progress: 0.0,
                                queued: false,
                                paused: false,
                                complete: false,
                                speed: 0,
                                eta: None,
                                error: Some(if reason.is_empty() {
                                    "SABnzbd reports the job as failed".to_owned()
                                } else {
                                    reason.to_owned()
                                }),
                            }
                        }
                        _ => continue,
                    };
                    map.insert(key.clone(), observation);
                }
            }
            Some(map)
        }
    }
}

/// Resolves the stall limit in seconds: the `UNSEENARR_SETTING_DOWNLOADS_`
/// override wins, then the settings table, then the default. Negatives clamp
/// to 0, which turns the stall check off.
async fn stalled_timeout_seconds(state: &AppState) -> i64 {
    let minutes = if let Some(raw) = state.settings_overrides.get(STALLED_KEY) {
        raw.expose()
            .trim()
            .parse::<i64>()
            .unwrap_or(DEFAULT_STALLED_MINUTES)
    } else {
        db::settings::get_i64(&state.db, STALLED_KEY)
            .await
            .ok()
            .flatten()
            .unwrap_or(DEFAULT_STALLED_MINUTES)
    };
    minutes.max(0) * 60
}

/// Reconciles active downloads with their clients once, before serving.
pub async fn reconcile_on_startup(state: &AppState) {
    let mut poller = Poller::new();
    let now = chrono::Utc::now().timestamp();
    match poller.poll(state, now, 1).await {
        Ok(summary) => {
            tracing::info!(
                changed = summary.changed,
                failed = summary.failed,
                "downloads reconciled on startup"
            );
        }
        Err(error) => {
            tracing::warn!(%error, "downloads reconcile on startup failed");
        }
    }
}

/// Polls every 5 s while downloads are active, every 60 s when idle.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut poller = Poller::new();
        loop {
            let now = chrono::Utc::now().timestamp();
            let active = match poller.poll(&state, now, 2).await {
                Ok(summary) => summary.active,
                Err(error) => {
                    tracing::warn!(%error, "download poll failed");
                    false
                }
            };
            tokio::time::sleep(Duration::from_secs(if active { 5 } else { 60 })).await;
        }
    });
}
