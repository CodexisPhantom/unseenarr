//! Integration tests for the download poller (issue #61). No HTTP router
//! needed: [`api::downloads::Poller`] is driven directly with explicit epoch
//! seconds.

use api::AppState;
use api::downloads::Poller;
use better_auth::seaorm::sea_orm::{ActiveModelTrait, EntityTrait, Set};
use db::downloads::NewDownload;
use db::entities::{quality_profiles, root_folders};
use db::movies::{Metadata, NewMovie};
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TORRENT_HASH: &str = "8c212779b4abde7c6bc608063a0d008b7e40ce32";
const CATEGORY: &str = "unseenarr";

struct App {
    state: AppState,
    _dir: TempDir,
}

impl App {
    async fn new() -> Self {
        let (db, dir) = db::testing::temp_db().await;
        let state = AppState::new(db, "test secret", None).await.unwrap();
        Self { state, _dir: dir }
    }

    async fn seed_movie(&self) -> (i32, i32) {
        let folder = root_folders::ActiveModel {
            path: Set("/movies".into()),
            created_at: Set(db::now()),
            ..Default::default()
        }
        .insert(&self.state.db)
        .await
        .unwrap();
        let folder_id = folder.id;
        let profile = quality_profiles::Entity::find()
            .one(&self.state.db)
            .await
            .unwrap()
            .unwrap();
        let movie = db::movies::create(
            &self.state.db,
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
                root_folder_id: folder_id,
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
        .id;
        (movie, folder_id)
    }

    async fn qb_client(&self, server_uri: &str, enabled: bool) -> i32 {
        db::download_clients::create(
            &self.state.db,
            db::download_clients::Values {
                name: "qBit".into(),
                kind: "qbittorrent".into(),
                url: server_uri.into(),
                username: Some("admin".into()),
                password: Some(db::secret::encrypt(
                    &self.state.secrets,
                    &db::secret::Secret::new("pw"),
                )),
                api_key: None,
                category: CATEGORY.into(),
                remote_path: None,
                local_path: None,
                enabled,
                tags: Vec::new(),
            },
        )
        .await
        .unwrap()
        .model
        .id
    }

    async fn sab_client(&self, server_uri: &str) -> i32 {
        db::download_clients::create(
            &self.state.db,
            db::download_clients::Values {
                name: "sab".into(),
                kind: "sabnzbd".into(),
                url: server_uri.into(),
                username: None,
                password: None,
                api_key: Some(db::secret::encrypt(
                    &self.state.secrets,
                    &db::secret::Secret::new("key"),
                )),
                category: CATEGORY.into(),
                remote_path: None,
                local_path: None,
                enabled: true,
                tags: Vec::new(),
            },
        )
        .await
        .unwrap()
        .model
        .id
    }

    async fn download(&self, movie_id: i32, client_id: Option<i32>, key: &str, state: &str) -> i32 {
        db::downloads::create(
            &self.state.db,
            NewDownload {
                movie_id,
                movie_profile_id: None,
                indexer_id: None,
                download_client_id: client_id,
                client_download_id: key.into(),
                release_title: "The.Matrix.1999.1080p.BluRay".into(),
                quality_id: 7,
                custom_format_score: 0,
                size: 10_000_000_000,
                state: state.into(),
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn state_of(&self, id: i32) -> (String, Option<String>, Option<String>) {
        let row = db::downloads::find(&self.state.db, id)
            .await
            .unwrap()
            .unwrap();
        (row.state, row.error, row.completed_at)
    }
}

async fn mount_qb_login(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "SID=abc123; HttpOnly; path=/")
                .set_body_string("Ok."),
        )
        .mount(server)
        .await;
}

fn qb_torrent(progress: f64, state: &str) -> serde_json::Value {
    json!([{
        "hash": TORRENT_HASH,
        "name": "The.Matrix.1999.1080p.BluRay",
        "category": CATEGORY,
        "state": state,
        "progress": progress,
        "size": 10_000_000_000_i64,
        "amount_left": 5_000_000_000_i64,
        "dlspeed": 1_000_000,
        "eta": 120,
    }])
}

async fn mount_qb_torrents(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .and(query_param("category", CATEGORY))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn queued_moves_to_downloading_and_stays_there() {
    let app = App::new().await;
    let server = MockServer::start().await;
    mount_qb_login(&server).await;
    mount_qb_torrents(&server, qb_torrent(0.1, "downloading")).await;

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.qb_client(&server.uri(), true).await;
    let id = app
        .download(movie_id, Some(client_id), TORRENT_HASH, "queued")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 1000, 2).await.unwrap();
    assert_eq!(app.state_of(id).await.0, "downloading");

    let mut poller = Poller::new();
    poller.poll(&app.state, 2000, 2).await.unwrap();
    assert_eq!(app.state_of(id).await.0, "downloading");
}

#[tokio::test]
async fn missing_fails_on_the_second_poll() {
    let app = App::new().await;
    let server = MockServer::start().await;
    mount_qb_login(&server).await;
    mount_qb_torrents(&server, json!([])).await;

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.qb_client(&server.uri(), true).await;
    let id = app
        .download(movie_id, Some(client_id), TORRENT_HASH, "queued")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 1000, 2).await.unwrap();
    let (state, error, _) = app.state_of(id).await;
    assert_eq!(state, "queued");
    assert_eq!(error, None);

    poller.poll(&app.state, 2000, 2).await.unwrap();
    let (state, error, _) = app.state_of(id).await;
    assert_eq!(state, "failed");
    assert!(error.unwrap().contains("no longer"));
}

#[tokio::test]
async fn startup_reconcile_fails_missing_on_the_first_poll() {
    let app = App::new().await;
    let server = MockServer::start().await;
    mount_qb_login(&server).await;
    mount_qb_torrents(&server, json!([])).await;

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.qb_client(&server.uri(), true).await;
    let id = app
        .download(movie_id, Some(client_id), TORRENT_HASH, "queued")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 1000, 1).await.unwrap();
    let (state, error, _) = app.state_of(id).await;
    assert_eq!(state, "failed");
    assert!(error.unwrap().contains("no longer"));
}

#[tokio::test]
async fn unreachable_client_keeps_state_and_error() {
    let app = App::new().await;
    let server = MockServer::start().await;
    mount_qb_login(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.qb_client(&server.uri(), true).await;
    let id = app
        .download(movie_id, Some(client_id), TORRENT_HASH, "downloading")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 1000, 2).await.unwrap();
    poller.poll(&app.state, 2000, 2).await.unwrap();
    let (state, error, _) = app.state_of(id).await;
    assert_eq!(state, "downloading");
    assert_eq!(error, None);
}

#[tokio::test]
async fn stalled_download_fails_past_the_limit() {
    let app = App::new().await;
    let server = MockServer::start().await;
    mount_qb_login(&server).await;
    mount_qb_torrents(&server, qb_torrent(0.4, "downloading")).await;
    db::settings::set_i64(&app.state.db, "downloads.stalled_timeout_minutes", 60)
        .await
        .unwrap();

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.qb_client(&server.uri(), true).await;
    let id = app
        .download(movie_id, Some(client_id), TORRENT_HASH, "downloading")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 0, 2).await.unwrap();
    assert_eq!(app.state_of(id).await.0, "downloading");
    poller.poll(&app.state, 3600, 2).await.unwrap();
    assert_eq!(app.state_of(id).await.0, "downloading");
    poller.poll(&app.state, 3601, 2).await.unwrap();
    let (state, error, _) = app.state_of(id).await;
    assert_eq!(state, "failed");
    assert!(error.unwrap().contains("no progress"));
}

#[tokio::test]
async fn paused_download_survives_the_stall_limit() {
    let app = App::new().await;
    let server = MockServer::start().await;
    mount_qb_login(&server).await;
    mount_qb_torrents(&server, qb_torrent(0.4, "pausedDL")).await;
    db::settings::set_i64(&app.state.db, "downloads.stalled_timeout_minutes", 60)
        .await
        .unwrap();

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.qb_client(&server.uri(), true).await;
    let id = app
        .download(movie_id, Some(client_id), TORRENT_HASH, "downloading")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 0, 2).await.unwrap();
    assert_eq!(app.state_of(id).await.0, "paused");
    poller.poll(&app.state, 10_000_000, 2).await.unwrap();
    let (state, error, _) = app.state_of(id).await;
    assert_eq!(state, "paused");
    assert_eq!(error, None);
}

#[tokio::test]
async fn finished_torrent_completes() {
    let app = App::new().await;
    let server = MockServer::start().await;
    mount_qb_login(&server).await;
    mount_qb_torrents(&server, qb_torrent(1.0, "uploading")).await;

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.qb_client(&server.uri(), true).await;
    let id = app
        .download(movie_id, Some(client_id), TORRENT_HASH, "downloading")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 1000, 2).await.unwrap();
    let (state, _, completed_at) = app.state_of(id).await;
    assert_eq!(state, "completed");
    assert!(completed_at.is_some());
}

#[tokio::test]
async fn errored_torrent_fails() {
    let app = App::new().await;
    let server = MockServer::start().await;
    mount_qb_login(&server).await;
    mount_qb_torrents(&server, qb_torrent(0.2, "error")).await;

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.qb_client(&server.uri(), true).await;
    let id = app
        .download(movie_id, Some(client_id), TORRENT_HASH, "queued")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 1000, 2).await.unwrap();
    let (state, error, _) = app.state_of(id).await;
    assert_eq!(state, "failed");
    assert!(error.unwrap().contains("qBittorrent"));
}

#[tokio::test]
async fn disabled_client_is_not_polled() {
    let app = App::new().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(0)
        .mount(&server)
        .await;

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.qb_client(&server.uri(), false).await;
    let id = app
        .download(movie_id, Some(client_id), TORRENT_HASH, "queued")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 1000, 2).await.unwrap();
    let (state, error, _) = app.state_of(id).await;
    assert_eq!(state, "queued");
    assert_eq!(error, None);
}

#[tokio::test]
async fn sabnzbd_history_resolves_finished_jobs() {
    let app = App::new().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("mode", "queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "queue": { "slots": [] } })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("mode", "history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "history": {
                "noofslots": 2,
                "slots": [
                    {
                        "nzo_id": "SABnzbd_nzo_done",
                        "name": "Movie.2024",
                        "category": CATEGORY,
                        "status": "Completed",
                        "bytes": 1000,
                        "storage": "/downloads/done",
                        "path": "/downloads/done",
                        "fail_message": "",
                    },
                    {
                        "nzo_id": "SABnzbd_nzo_bad",
                        "name": "Movie.2023",
                        "category": CATEGORY,
                        "status": "Failed",
                        "bytes": 1000,
                        "storage": "",
                        "path": "",
                        "fail_message": "Unpacking failed",
                    },
                ],
            },
        })))
        .mount(&server)
        .await;

    let (movie_id, _) = app.seed_movie().await;
    let client_id = app.sab_client(&server.uri()).await;
    let done = app
        .download(movie_id, Some(client_id), "SABnzbd_nzo_done", "downloading")
        .await;
    let bad = app
        .download(movie_id, Some(client_id), "SABnzbd_nzo_bad", "downloading")
        .await;

    let mut poller = Poller::new();
    poller.poll(&app.state, 1000, 2).await.unwrap();
    let (state, _, completed_at) = app.state_of(done).await;
    assert_eq!(state, "completed");
    assert!(completed_at.is_some());
    let (state, error, _) = app.state_of(bad).await;
    assert_eq!(state, "failed");
    assert!(error.unwrap().contains("Unpacking failed"));
}
