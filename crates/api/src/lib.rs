//! HTTP API: routes, the OpenAPI document and the Axum router.

pub mod access;
mod api_keys;
pub mod audit;
pub mod auth;
pub mod base_path;
mod collections;
pub mod compat;
mod custom_formats;
mod download_clients;
pub mod error;
pub mod events;
mod grab;
mod health;
pub mod import_sources;
mod indexers;
mod libraries;
mod library;
pub mod logs;
mod lookup;
mod me;
mod movies;
mod naming_settings;
mod owner;
mod profiles;
mod proxy;
mod quality_definitions;
mod queue;
pub mod radarr;
mod radarr_movies;
mod releases;
mod roles;
mod root_folders;
pub mod setup;
pub mod sonarr;
mod sonarr_series;
mod subtitles;
mod tags;
mod tmdb;
mod users;
mod web;

use std::path::PathBuf;

use axum::Router;
use axum::extract::FromRef;
use better_auth::integrations::axum::AxumIntegration;
use better_auth::{AuthError, AuthResult};
use db::DatabaseConnection;
use unseenarr_core::permission::Permission;
use utoipa::OpenApi;
use utoipa_axum::routes;

use crate::access::ApiRouter;
use crate::error::Problem;

#[derive(Clone)]
pub struct AppState {
    pub events: events::EventBus,
    pub db: DatabaseConnection,
    /// The mount prefix from UNSEENARR_BASE_URL; empty at the root.
    pub base_path: String,
    pub auth: auth::Auth,
    pub policy: auth::Policy,
    pub setup: setup::Setup,
    /// Encrypts the integration secrets (ADR 0011).
    pub secrets: db::secret::SecretKey,
    /// The client for TMDB and the other metadata calls.
    pub http: unseenarr_core::http::OutboundClient,
    /// Tests point this at a mock server.
    pub tmdb_base_url: String,
    /// Settings given as `UNSEENARR_SETTING_<KEY>`: they win over the table and
    /// cannot be changed through the API (docs/10-configuration.md).
    pub settings_overrides: db::settings::Overrides,
    /// Directory holding daily JSONL log files (`unseenarr-YYYY-MM-DD.log`).
    pub logs_dir: PathBuf,
    /// Short-lived cache of manual search results, keyed by release id, so
    /// grab requests can look up a searched release without re-querying.
    pub search_cache: unseenarr_core::search::SearchCache,
    /// Serializes grabs per movie profile, so one profile gets one download.
    pub(crate) grabs: grab::ProfileLocks,
    /// The import sources whose import is running.
    pub(crate) import_runs: import_sources::RunSet,
    /// UTC ISO 8601 timestamp recorded when the server state was created.
    pub started_at: String,
}

impl AppState {
    /// Tells connected clients that a movie row changed.
    pub fn publish_movie_changed(&self, id: i32) {
        self.publish_movie_changed_scoped(id, None);
    }

    /// Tells connected clients with access to `library_id` that a movie row
    /// changed.
    pub fn publish_movie_changed_in_library(&self, id: i32, library_id: i32) {
        self.publish_movie_changed_scoped(id, Some(library_id));
    }

    fn publish_movie_changed_scoped(&self, id: i32, library_id: Option<i32>) {
        self.events.publish(crate::events::AppEvent {
            kind: "movie.updated".to_owned(),
            data: serde_json::json!({ "id": id }),
            permission: Some(unseenarr_core::permission::Permission::LibraryRead),
            library_id,
            target_user: None,
            target_api_key: None,
        });
    }

    /// Re-reads grants for one user: sent only to that user.
    pub fn publish_permissions_changed(&self, user_id: &str) {
        self.events.publish(crate::events::AppEvent {
            kind: "permissions.changed".to_owned(),
            data: serde_json::Value::String(user_id.to_owned()),
            permission: None,
            library_id: None,
            target_user: Some(user_id.to_owned()),
            target_api_key: None,
        });
    }

    /// A download's progress moved, visible to `queue:read` holders.
    pub fn publish_download_progress(&self, id: i32, progress: f64) {
        self.publish_download_progress_scoped(id, progress, None);
    }

    /// A download's progress moved, visible to `queue:read` holders in
    /// `library_id`.
    pub fn publish_download_progress_in_library(&self, id: i32, progress: f64, library_id: i32) {
        self.publish_download_progress_scoped(id, progress, Some(library_id));
    }

    fn publish_download_progress_scoped(&self, id: i32, progress: f64, library_id: Option<i32>) {
        self.events.publish(crate::events::AppEvent {
            kind: "download.progress".to_owned(),
            data: serde_json::json!({ "id": id, "progress": progress }),
            permission: Some(unseenarr_core::permission::Permission::QueueRead),
            library_id,
            target_user: None,
            target_api_key: None,
        });
    }

    /// A download changed state, visible to `queue:read` holders.
    pub fn publish_download_state(&self, id: i32, state: &str) {
        self.publish_download_state_scoped(id, state, None);
    }

    /// A download changed state, visible to `queue:read` holders in
    /// `library_id`.
    pub fn publish_download_state_in_library(&self, id: i32, state: &str, library_id: i32) {
        self.publish_download_state_scoped(id, state, Some(library_id));
    }

    fn publish_download_state_scoped(&self, id: i32, state: &str, library_id: Option<i32>) {
        self.events.publish(crate::events::AppEvent {
            kind: "download.state".to_owned(),
            data: serde_json::json!({ "id": id, "state": state }),
            permission: Some(unseenarr_core::permission::Permission::QueueRead),
            library_id,
            target_user: None,
            target_api_key: None,
        });
    }

    /// Detailed download progress event (docs/05-api.md), visible to `queue:read` holders.
    pub fn publish_download_progress_update(
        &self,
        id: i32,
        progress: f64,
        speed: i64,
        eta: Option<i64>,
        state: &str,
    ) {
        self.publish_download_progress_update_scoped(id, progress, speed, eta, state, None);
    }

    /// Detailed download progress event scoped to `library_id`.
    pub fn publish_download_progress_update_in_library(
        &self,
        id: i32,
        progress: f64,
        speed: i64,
        eta: Option<i64>,
        state: &str,
        library_id: i32,
    ) {
        self.publish_download_progress_update_scoped(
            id,
            progress,
            speed,
            eta,
            state,
            Some(library_id),
        );
    }

    fn publish_download_progress_update_scoped(
        &self,
        id: i32,
        progress: f64,
        speed: i64,
        eta: Option<i64>,
        state: &str,
        library_id: Option<i32>,
    ) {
        self.events.publish(crate::events::AppEvent {
            kind: "download.progress".to_owned(),
            data: serde_json::json!({
                "id": id,
                "progress": progress,
                "speed": speed,
                "eta": eta,
                "state": state,
            }),
            permission: Some(unseenarr_core::permission::Permission::QueueRead),
            library_id,
            target_user: None,
            target_api_key: None,
        });
    }

    /// Download state transition event (docs/05-api.md), visible to `queue:read` holders.
    pub fn publish_download_state_changed(&self, id: i32, old_state: &str, new_state: &str) {
        self.publish_download_state_changed_scoped(id, old_state, new_state, None);
    }

    /// Download state transition event scoped to `library_id`.
    pub fn publish_download_state_changed_in_library(
        &self,
        id: i32,
        old_state: &str,
        new_state: &str,
        library_id: i32,
    ) {
        self.publish_download_state_changed_scoped(id, old_state, new_state, Some(library_id));
    }

    fn publish_download_state_changed_scoped(
        &self,
        id: i32,
        old_state: &str,
        new_state: &str,
        library_id: Option<i32>,
    ) {
        self.events.publish(crate::events::AppEvent {
            kind: "download.state".to_owned(),
            data: serde_json::json!({
                "id": id,
                "oldState": old_state,
                "newState": new_state,
            }),
            permission: Some(unseenarr_core::permission::Permission::QueueRead),
            library_id,
            target_user: None,
            target_api_key: None,
        });
    }

    /// A background task changed status, visible to `system:tasks` holders.
    pub fn publish_task_status(&self, name: &str, status: &str) {
        self.events.publish(crate::events::AppEvent {
            kind: "task.status".to_owned(),
            data: serde_json::json!({ "name": name, "status": status }),
            permission: Some(unseenarr_core::permission::Permission::SystemTasks),
            library_id: None,
            target_user: None,
            target_api_key: None,
        });
    }
    /// `db` must be migrated: the auth instance reads its tables.
    pub async fn new(
        db: DatabaseConnection,
        secret: &str,
        base_url: Option<&str>,
    ) -> AuthResult<Self> {
        let base_path = base_path::from_url(base_url);
        let (auth, policy) = auth::build(db.clone(), secret, base_url).await?;
        let disabled = db::users::disabled_ids(&db)
            .await
            .map_err(|error| AuthError::internal(format!("load disabled users: {error}")))?;
        for id in &disabled {
            policy.disable(id);
        }
        Ok(Self {
            events: events::EventBus::new(),
            db,
            base_path,
            auth,
            policy,
            setup: setup::Setup::default(),
            secrets: db::secret::SecretKey::derive(secret),
            http: unseenarr_core::http::OutboundClient::new()
                .map_err(|error| AuthError::internal(format!("build http client: {error}")))?,
            tmdb_base_url: unseenarr_core::tmdb::DEFAULT_BASE_URL.to_owned(),
            settings_overrides: db::settings::Overrides::from_env(),
            logs_dir: std::env::temp_dir().join("unseenarr-logs-default"),
            search_cache: unseenarr_core::search::SearchCache::new(),
            grabs: grab::ProfileLocks::default(),
            import_runs: import_sources::RunSet::default(),
            started_at: db::now(),
        })
    }

    /// Sets the directory used for reading and downloading daily log files.
    #[must_use]
    pub fn with_logs_dir(mut self, dir: PathBuf) -> Self {
        self.logs_dir = dir;
        self
    }
}

/// The library's extractors and router find the auth instance through this.
impl FromRef<AppState> for auth::Auth {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

#[derive(OpenApi)]
#[openapi(info(title = "Unseenarr API"), components(schemas(Problem)))]
struct ApiDoc;

/// Every API route is registered here once with its access rule, so the
/// router, runtime enforcement and `x-permission` in OpenAPI cannot disagree.
pub fn routes() -> ApiRouter {
    ApiRouter::with_openapi(ApiDoc::openapi())
        .public(routes!(health::health))
        .public(routes!(setup::status, setup::create))
        .authenticated(routes!(me::me))
        .authenticated(routes!(events::stream_events))
        .authenticated(routes!(owner::transfer))
        .authenticated(routes!(roles::list_permissions))
        .authenticated(routes!(api_keys::list_own, api_keys::create_own))
        .authenticated(routes!(api_keys::revoke_own))
        .require(Permission::ApikeysManage, routes!(api_keys::list_all))
        .require(Permission::ApikeysManage, routes!(api_keys::revoke_any))
        .require(
            Permission::UsersManage,
            routes!(roles::list_assignable_roles),
        )
        .require(
            Permission::UsersManage,
            routes!(libraries::list_assignable_libraries),
        )
        .require(
            Permission::RolesManage,
            routes!(roles::list_roles, roles::create_role),
        )
        .require(
            Permission::RolesManage,
            routes!(roles::update_role, roles::delete_role),
        )
        .require(
            Permission::UsersManage,
            routes!(users::list_users, users::create_user),
        )
        .require(
            Permission::UsersManage,
            routes!(users::update_user, users::delete_user),
        )
        .require(
            Permission::UsersManage,
            routes!(users::set_user_permissions),
        )
        .require(Permission::SystemAudit, routes!(audit::list))
        .require(Permission::SystemLogs, routes!(logs::list))
        .require(Permission::SystemLogs, routes!(logs::download))
        .require(Permission::ProfilesRead, routes!(profiles::list_profiles))
        .require(
            Permission::ProfilesManage,
            routes!(profiles::create_profile),
        )
        .require(Permission::ProfilesRead, routes!(profiles::get_profile))
        .require(
            Permission::ProfilesManage,
            routes!(profiles::update_profile, profiles::delete_profile),
        )
        .require(
            Permission::ProfilesRead,
            routes!(custom_formats::list_custom_formats),
        )
        .require(
            Permission::ProfilesManage,
            routes!(custom_formats::create_custom_format),
        )
        .require(
            Permission::ProfilesManage,
            routes!(custom_formats::import_custom_formats),
        )
        .require(
            Permission::ProfilesRead,
            routes!(custom_formats::export_custom_formats),
        )
        .require(
            Permission::ProfilesRead,
            routes!(custom_formats::test_custom_formats),
        )
        .require(
            Permission::ProfilesRead,
            routes!(custom_formats::get_custom_format),
        )
        .require(
            Permission::ProfilesRead,
            routes!(custom_formats::export_custom_format),
        )
        .require(
            Permission::ProfilesManage,
            routes!(
                custom_formats::update_custom_format,
                custom_formats::delete_custom_format
            ),
        )
        .require(Permission::ImportRun, routes!(library::scan))
        .require(Permission::ImportRun, routes!(library::adopt))
        .require(
            Permission::ImportRun,
            routes!(import_sources::list_sources, import_sources::create_source),
        )
        .require(
            Permission::ImportRun,
            routes!(import_sources::test_source, import_sources::order_sources),
        )
        .require(
            Permission::ImportRun,
            routes!(import_sources::delete_source),
        )
        .require(
            Permission::ImportRun,
            routes!(import_sources::preview_source),
        )
        .require(Permission::ImportRun, routes!(import_sources::run_source))
        .require(Permission::LibraryAdd, routes!(lookup::lookup))
        .require(Permission::LibraryRead, routes!(movies::list_movies))
        .require(Permission::LibraryAdd, routes!(movies::create_movie))
        .require(Permission::LibraryRead, routes!(movies::get_movie))
        .require(Permission::LibraryEdit, routes!(movies::update_movie))
        .require(Permission::LibraryDelete, routes!(movies::delete_movie))
        .require(Permission::LibraryAdd, routes!(movies::add_profile))
        .require(Permission::LibraryDelete, routes!(movies::remove_profile))
        .require(Permission::LibraryRead, routes!(movies::list_files))
        .require(Permission::LibraryDelete, routes!(movies::delete_file))
        .require(Permission::SearchRun, routes!(releases::search_movie))
        .require(Permission::ReleaseGrab, routes!(grab::grab_release))
        .require(
            Permission::LibraryRead,
            routes!(subtitles::list_language_profiles),
        )
        .require(
            Permission::SubtitlesManage,
            routes!(subtitles::create_language_profile),
        )
        .require(
            Permission::SubtitlesManage,
            routes!(
                subtitles::update_language_profile,
                subtitles::delete_language_profile
            ),
        )
        .require(
            Permission::LibraryRead,
            routes!(subtitles::list_movie_subtitles),
        )
        .require(
            Permission::SubtitlesManage,
            routes!(subtitles::set_movie_language_profile),
        )
        .require(
            Permission::SubtitlesManage,
            routes!(subtitles::scan_subtitles),
        )
        .require(Permission::LibraryRead, routes!(subtitles::get_settings))
        .require(
            Permission::SubtitlesManage,
            routes!(subtitles::update_settings),
        )
        .require(
            Permission::LibraryRead,
            routes!(collections::list_collections),
        )
        .require(
            Permission::LibraryRead,
            routes!(collections::get_collection),
        )
        .require(
            Permission::LibraryEdit,
            routes!(collections::update_collection),
        )
        .require(Permission::LibraryRead, routes!(tags::list_tags))
        .require(
            Permission::LibraryEdit,
            routes!(tags::create_tag, tags::update_tag, tags::delete_tag),
        )
        .require(
            Permission::ProfilesRead,
            routes!(quality_definitions::list_definitions),
        )
        .require(
            Permission::ProfilesManage,
            routes!(quality_definitions::update_definition),
        )
        .require(Permission::SettingsRead, routes!(tmdb::get_settings))
        .require(Permission::SettingsManage, routes!(tmdb::update_settings))
        .require(
            Permission::SettingsRead,
            routes!(naming_settings::get_naming, naming_settings::preview_naming),
        )
        .require(
            Permission::SettingsManage,
            routes!(naming_settings::update_naming),
        )
        .require(Permission::SettingsRead, routes!(proxy::get_settings))
        .require(
            Permission::SettingsManage,
            routes!(proxy::update_settings, proxy::test_proxy),
        )
        .require(Permission::SettingsRead, routes!(libraries::list_libraries))
        .require(
            Permission::SettingsManage,
            routes!(libraries::create_library),
        )
        .require(
            Permission::SettingsManage,
            routes!(libraries::update_library, libraries::delete_library),
        )
        .require(
            Permission::SettingsRead,
            routes!(root_folders::list_root_folders),
        )
        .require(
            Permission::SettingsManage,
            routes!(root_folders::create_root_folder),
        )
        .require(
            Permission::SettingsManage,
            routes!(
                root_folders::update_root_folder,
                root_folders::delete_root_folder
            ),
        )
        .require(Permission::SettingsManage, routes!(root_folders::browse))
        .require(Permission::IndexersRead, routes!(indexers::list_indexers))
        .require(
            Permission::IndexersManage,
            routes!(indexers::create_indexer),
        )
        .require(Permission::IndexersRead, routes!(indexers::get_indexer))
        .require(
            Permission::IndexersManage,
            routes!(
                indexers::update_indexer,
                indexers::delete_indexer,
                indexers::test_indexer
            ),
        )
        .require(
            Permission::ClientsRead,
            routes!(download_clients::list_clients),
        )
        .require(
            Permission::ClientsManage,
            routes!(download_clients::create_client),
        )
        .require(
            Permission::ClientsManage,
            routes!(
                download_clients::update_client,
                download_clients::delete_client,
                download_clients::test_client
            ),
        )
        .require(Permission::QueueRead, routes!(queue::list_queue))
        .require(Permission::QueueManage, routes!(queue::remove_queue_item))
}

/// Every Radarr v3 compatibility route, registered once with its permission.
/// Not included in Unseenarr's OpenAPI document.
pub fn radarr_routes() -> compat::CompatRouter {
    radarr::routes()
}

/// Every Sonarr v3 compatibility route, registered once with its permission.
/// Not included in Unseenarr's OpenAPI document.
pub fn sonarr_routes() -> compat::CompatRouter {
    sonarr::routes()
}

/// The OpenAPI document. `just gen` writes it to `openapi.json`.
pub fn openapi() -> utoipa::openapi::OpenApi {
    routes().into_openapi()
}

pub fn router(state: AppState) -> Router {
    let base_path = state.base_path.clone();
    let api = routes().into_router(&state);
    // The auth routes are the library's own; they are not in our OpenAPI
    // document.
    let auth =
        state
            .auth
            .clone()
            .axum_router_with_state()
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                audit::auth_events,
            ));
    let radarr = radarr_routes().into_service(&state);
    let radarr_fallback = radarr.clone();
    let sonarr = sonarr_routes().into_service(&state);
    let sonarr_fallback = sonarr.clone();
    let web_state = state.clone();
    let app = api
        .nest("/api/auth", auth)
        .nest_service(radarr::API_BASE, radarr)
        .nest_service(sonarr::API_BASE, sonarr)
        .fallback(move |req: axum::extract::Request| {
            let radarr = radarr_fallback.clone();
            let sonarr = sonarr_fallback.clone();
            async move {
                let req = match compat::dispatch_prefix(req, radarr::API_BASE) {
                    Ok(rewritten) => {
                        return tower::ServiceExt::oneshot(radarr, rewritten)
                            .await
                            .unwrap_or_else(|never| match never {});
                    }
                    Err(req) => req,
                };
                match compat::dispatch_prefix(req, sonarr::API_BASE) {
                    Ok(rewritten) => tower::ServiceExt::oneshot(sonarr, rewritten)
                        .await
                        .unwrap_or_else(|never| match never {}),
                    Err(req) => {
                        web::serve(axum::extract::State(web_state.clone()), req.uri().clone()).await
                    }
                }
            }
        })
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            setup::gate,
        ))
        .with_state(state);
    if base_path.is_empty() {
        app
    } else {
        Router::new().nest(&base_path, app)
    }
}
