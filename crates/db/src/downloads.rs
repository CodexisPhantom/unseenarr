//! Active downloads and release blocklist (docs/04-database.md,
//! docs/06-state-machines.md).

use std::collections::HashMap;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder,
    Set, TransactionTrait,
};

use crate::entities::{blocklist, download_clients, downloads, movies};
use unseenarr_core::download::{DownloadState, transition};

/// Terminal download states that no longer belong in the active queue.
pub const FINAL_STATES: &[&str] = &["imported", "failed", "removed"];

/// Whether `state` is terminal and excluded from the active queue.
#[must_use]
pub fn is_final_state(state: &str) -> bool {
    FINAL_STATES.contains(&state)
}
/// Whether a movie profile already has a download outside the final states,
/// which a new grab would duplicate (docs/06-state-machines.md).
pub async fn has_active_for_profile(
    db: &DatabaseConnection,
    movie_profile_id: i32,
) -> Result<bool, DbErr> {
    Ok(downloads::Entity::find()
        .filter(downloads::Column::MovieProfileId.eq(movie_profile_id))
        .filter(downloads::Column::State.is_not_in(FINAL_STATES.iter().copied()))
        .one(db)
        .await?
        .is_some())
}

/// An active download row enriched with its movie title/year and client name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedDownload {
    pub download: downloads::Model,
    /// `download.movie_id` unwrapped: the queue only lists movie downloads.
    pub movie_id: i32,
    pub movie_title: String,
    pub movie_year: i32,
    pub movie_root_folder_id: i32,
    pub client: Option<download_clients::Model>,
}

/// Values to insert a new download row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDownload {
    pub movie_id: i32,
    pub movie_profile_id: Option<i32>,
    pub indexer_id: Option<i32>,
    pub download_client_id: Option<i32>,
    pub client_download_id: String,
    pub release_title: String,
    pub quality_id: i32,
    pub custom_format_score: i32,
    pub size: i64,
    pub state: String,
}

/// Inserts a new download row and returns it.
pub async fn create(db: &DatabaseConnection, new: NewDownload) -> Result<downloads::Model, DbErr> {
    downloads::ActiveModel {
        movie_id: Set(Some(new.movie_id)),
        show_id: Set(None),
        movie_profile_id: Set(new.movie_profile_id),
        indexer_id: Set(new.indexer_id),
        download_client_id: Set(new.download_client_id),
        client_download_id: Set(new.client_download_id),
        release_title: Set(new.release_title),
        quality_id: Set(new.quality_id),
        custom_format_score: Set(new.custom_format_score),
        size: Set(new.size),
        state: Set(new.state),
        error: Set(None),
        attempts: Set(0),
        grabbed_at: Set(crate::now()),
        completed_at: Set(None),
        imported_at: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Loads one download by id.
pub async fn find(db: &DatabaseConnection, id: i32) -> Result<Option<downloads::Model>, DbErr> {
    downloads::Entity::find_by_id(id).one(db).await
}

/// Every non-final movie download, newest grabbed first, with its movie and client.
///
/// TV downloads are left out until the TV queue exists (#91).
pub async fn list_active(db: &DatabaseConnection) -> Result<Vec<QueuedDownload>, DbErr> {
    let rows = downloads::Entity::find()
        .filter(downloads::Column::State.is_not_in(FINAL_STATES.iter().copied()))
        .filter(downloads::Column::MovieId.is_not_null())
        .order_by_desc(downloads::Column::GrabbedAt)
        .order_by_desc(downloads::Column::Id)
        .all(db)
        .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let movie_ids: Vec<i32> = rows.iter().filter_map(|row| row.movie_id).collect();
    let movie_map: HashMap<i32, movies::Model> = movies::Entity::find()
        .filter(movies::Column::Id.is_in(movie_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|movie| (movie.id, movie))
        .collect();

    let client_ids: Vec<i32> = rows
        .iter()
        .filter_map(|row| row.download_client_id)
        .collect();
    let client_map: HashMap<i32, download_clients::Model> = if client_ids.is_empty() {
        HashMap::new()
    } else {
        download_clients::Entity::find()
            .filter(download_clients::Column::Id.is_in(client_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|client| (client.id, client))
            .collect()
    };

    Ok(rows
        .into_iter()
        .filter_map(|download| {
            let movie = movie_map.get(&download.movie_id?)?;
            let client = download
                .download_client_id
                .and_then(|id| client_map.get(&id).cloned());
            Some(QueuedDownload {
                movie_id: movie.id,
                movie_title: movie.title.clone(),
                movie_year: movie.year,
                movie_root_folder_id: movie.root_folder_id,
                client,
                download,
            })
        })
        .collect())
}

/// Marks a download as `removed` and optionally adds its release to `blocklist`
/// in one transaction. Returns the previous model, or `None` if not found or
/// already in a final state.
pub async fn mark_removed(
    db: &DatabaseConnection,
    id: i32,
    add_to_blocklist: bool,
    reason: &str,
) -> Result<Option<downloads::Model>, DbErr> {
    let txn = db.begin().await?;
    let Some(existing) = downloads::Entity::find_by_id(id).one(&txn).await? else {
        return Ok(None);
    };
    if is_final_state(&existing.state) {
        return Ok(None);
    }

    let mut active: downloads::ActiveModel = existing.clone().into();
    active.state = Set("removed".to_owned());
    active.update(&txn).await?;

    // TODO(#91): blocklist TV releases once `blocklist` can target a show.
    if add_to_blocklist && let Some(movie_id) = existing.movie_id {
        blocklist::ActiveModel {
            movie_id: Set(movie_id),
            release_title: Set(existing.release_title.clone()),
            indexer_id: Set(existing.indexer_id),
            reason: Set(reason.to_owned()),
            created_at: Set(crate::now()),
            ..Default::default()
        }
        .insert(&txn)
        .await?;
    }

    txn.commit().await?;
    Ok(Some(existing))
}

/// Moves a download to `next` along the steps the state machine allows and
/// records `error` with it. `Ok(None)` when the row is gone, is already in
/// `next`, or the move is not allowed; otherwise the row before and after.
pub async fn advance(
    db: &DatabaseConnection,
    id: i32,
    next: DownloadState,
    error: Option<String>,
) -> Result<Option<(downloads::Model, downloads::Model)>, DbErr> {
    let txn = db.begin().await?;
    let Some(existing) = downloads::Entity::find_by_id(id).one(&txn).await? else {
        return Ok(None);
    };
    let Some(current) = DownloadState::parse(&existing.state) else {
        return Err(DbErr::Custom(format!(
            "download {id} has an unreadable state"
        )));
    };
    if current == next {
        return Ok(None);
    }
    let Ok(path) = transition(current, next) else {
        return Ok(None);
    };
    // One write is one step: a longer route has an intermediate state that
    // has to be stored first.
    if path.len() != 1 {
        return Ok(None);
    }
    let mut active: downloads::ActiveModel = existing.clone().into();
    active.state = Set(next.as_str().to_owned());
    active.error = Set(error);
    if path.contains(&DownloadState::Completed) && existing.completed_at.is_none() {
        active.completed_at = Set(Some(crate::now()));
    }
    let after = active.update(&txn).await?;
    txn.commit().await?;
    Ok(Some((existing, after)))
}

/// Lists blocklist rows for a movie, newest first.
pub async fn list_blocklist_for_movie(
    db: &DatabaseConnection,
    movie_id: i32,
) -> Result<Vec<blocklist::Model>, DbErr> {
    blocklist::Entity::find()
        .filter(blocklist::Column::MovieId.eq(movie_id))
        .order_by_desc(blocklist::Column::Id)
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{quality_profiles, root_folders};
    use crate::movies::{Metadata, NewMovie};
    use crate::testing::temp_db;

    async fn seed_movie(db: &DatabaseConnection) -> i32 {
        let folder = root_folders::ActiveModel {
            path: Set("/movies".into()),
            created_at: Set(crate::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        let profile = quality_profiles::Entity::find()
            .one(db)
            .await
            .unwrap()
            .unwrap();
        crate::movies::create(
            db,
            NewMovie {
                metadata: Metadata {
                    tmdb_id: 603,
                    imdb_id: Some("tt0133093".into()),
                    title: "The Matrix".into(),
                    original_title: None,
                    sort_title: "matrix".into(),
                    year: 1999,
                    overview: None,
                    runtime: Some(136),
                    status: "released".into(),
                    in_cinemas: None,
                    digital_release: None,
                    physical_release: None,
                    poster_path: None,
                },
                collection_id: None,
                root_folder_id: folder.id,
                folder_name: "The Matrix (1999)".into(),
                monitored: true,
                minimum_availability: "released".into(),
            },
            &[profile.id],
            &[],
        )
        .await
        .unwrap()
        .model
        .id
    }

    #[tokio::test]
    async fn lists_active_downloads_and_removes_with_optional_blocklist() {
        let (db, _dir) = temp_db().await;
        let movie_id = seed_movie(&db).await;

        let active = create(
            &db,
            NewDownload {
                movie_id,
                movie_profile_id: None,
                indexer_id: None,
                download_client_id: None,
                client_download_id: "hash-active".into(),
                release_title: "The.Matrix.1999.1080p.BluRay".into(),
                quality_id: 7,
                custom_format_score: 0,
                size: 10_000_000_000,
                state: "downloading".into(),
            },
        )
        .await
        .unwrap();
        create(
            &db,
            NewDownload {
                movie_id,
                movie_profile_id: None,
                indexer_id: None,
                download_client_id: None,
                client_download_id: "hash-done".into(),
                release_title: "The.Matrix.1999.720p.BluRay".into(),
                quality_id: 6,
                custom_format_score: 0,
                size: 5_000_000_000,
                state: "imported".into(),
            },
        )
        .await
        .unwrap();

        let queued = list_active(&db).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].download.id, active.id);
        assert_eq!(queued[0].movie_title, "The Matrix");

        let removed = mark_removed(&db, active.id, true, "Removed from queue")
            .await
            .unwrap()
            .expect("should remove active download");
        assert_eq!(removed.state, "downloading");
        assert!(list_active(&db).await.unwrap().is_empty());

        let blocked = list_blocklist_for_movie(&db, movie_id).await.unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].release_title, "The.Matrix.1999.1080p.BluRay");
        assert_eq!(blocked[0].reason, "Removed from queue");

        // Removing again is a no-op returning None.
        assert!(
            mark_removed(&db, active.id, false, "Removed from queue")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn advance_follows_the_state_machine() {
        use unseenarr_core::download::DownloadState as S;
        let (db, _dir) = temp_db().await;
        let movie_id = seed_movie(&db).await;
        let download = create(
            &db,
            NewDownload {
                movie_id,
                movie_profile_id: None,
                indexer_id: None,
                download_client_id: None,
                client_download_id: "hash-advance".into(),
                release_title: "The.Matrix.1999.1080p.BluRay".into(),
                quality_id: 7,
                custom_format_score: 0,
                size: 10_000_000_000,
                state: "queued".into(),
            },
        )
        .await
        .unwrap();

        let (before, after) = advance(&db, download.id, S::Downloading, None)
            .await
            .unwrap()
            .expect("queued -> downloading moves");
        assert_eq!(before.state, "queued");
        assert_eq!(after.state, "downloading");

        // Downloading cannot jump straight to Imported.
        assert!(
            advance(&db, download.id, S::Imported, None)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            find(&db, download.id).await.unwrap().unwrap().state,
            "downloading"
        );

        let (_, after) = advance(&db, download.id, S::Completed, None)
            .await
            .unwrap()
            .expect("downloading -> completed moves");
        assert!(after.completed_at.is_some());

        let (_, after) = advance(&db, download.id, S::Importing, None)
            .await
            .unwrap()
            .expect("completed -> importing moves");
        assert_eq!(after.state, "importing");
        assert!(after.completed_at.is_some());
    }
}
