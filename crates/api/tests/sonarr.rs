//! Sonarr v3 compatibility API (`/sonarr/api/v3`): mount, API-key-only auth,
//! case-insensitive routing, per-route permissions, logged 501 fallback, and
//! the shared endpoints used by Seerr's Sonarr connection test
//! (docs/14-compatibility.md, section D "Sonarr endpoints (M8)").

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use api::compat::CompatRouteEntry;
use api::{AppState, router, sonarr_routes};
use arr_compat::sonarr::SONARR_VERSION;
use arr_compat::sonarr::dto::{
    EpisodeResource, LanguageProfileResource, QualityProfileResource, ReleaseType,
    RootFolderResource, SeriesResource, SystemResource, TagResource,
};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use better_auth::seaorm::sea_orm::{ActiveModelTrait, Set};
use db::entities::{
    episode_file_episodes, episode_files, episodes, role_permissions, roles, seasons, shows,
    user_role_libraries, user_roles,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;
use unseenarr_core::permission::Permission;
use unseenarr_core::quality::Quality as CoreQuality;

const HOST: &str = "unseenarr.test";
const ORIGIN: &str = "http://unseenarr.test";
const PASSWORD: &str = "correct horse";

#[derive(Debug, Clone)]
struct CapturedEvent {
    level: tracing::Level,
    fields: BTreeMap<String, String>,
}

#[derive(Clone, Default)]
struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl CaptureLayer {
    fn warns(&self) -> Vec<CapturedEvent> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.level == tracing::Level::WARN)
            .cloned()
            .collect()
    }
}

struct FieldVisitor(BTreeMap<String, String>);

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(BTreeMap::new());
        event.record(&mut visitor);
        self.events.lock().unwrap().push(CapturedEvent {
            level: *event.metadata().level(),
            fields: visitor.0,
        });
    }
}

struct Reply {
    status: StatusCode,
    content_type: String,
    cookies: Vec<String>,
    body: Value,
}

impl Reply {
    fn session_pair(&self) -> String {
        self.cookies
            .iter()
            .find(|cookie| cookie.contains("session_token="))
            .expect("a session cookie")
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }
}

struct App {
    router: Router,
    state: AppState,
    owner: String,
    _dir: TempDir,
}

impl App {
    async fn uninitialized(base_url: Option<&str>) -> Self {
        let (db, dir) = db::testing::temp_db().await;
        let state = AppState::new(db, "test secret", base_url).await.unwrap();
        let router = router(state.clone());
        Self {
            router,
            state,
            owner: String::new(),
            _dir: dir,
        }
    }

    async fn new() -> Self {
        Self::with_base_url(None).await
    }

    async fn with_base_url(base_url: Option<&str>) -> Self {
        let mut app = Self::uninitialized(base_url).await;
        let prefix = app.state.base_path.clone();
        let setup = json!({
            "name": "Owner",
            "email": "owner@example.com",
            "password": PASSWORD,
        });
        let reply = app
            .send(
                Request::post(format!("{prefix}/api/v1/setup"))
                    .header(header::HOST, HOST)
                    .header(header::ORIGIN, ORIGIN)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(setup.to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
        app.owner = reply.session_pair();
        app
    }

    async fn send(&self, request: Request<Body>) -> Reply {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_owned())
            .collect();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            content_type,
            cookies,
            body,
        }
    }

    async fn member(&self, email: &str, permissions: &[&str]) -> String {
        self.state.policy.admit_next_signup();
        let body = json!({ "name": "Member", "email": email, "password": PASSWORD });
        let reply = self
            .send(
                Request::post("/api/auth/sign-up/email")
                    .header(header::HOST, HOST)
                    .header(header::ORIGIN, ORIGIN)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let user_id = reply.body["user"]["id"].as_str().unwrap().to_owned();
        let cookie = reply.session_pair();

        let role = roles::ActiveModel {
            name: Set(format!("Role of {email}")),
            is_owner: Set(false),
            created_at: Set(db::now()),
            updated_at: Set(db::now()),
            ..Default::default()
        }
        .insert(&self.state.db)
        .await
        .unwrap();
        for permission in permissions {
            role_permissions::ActiveModel {
                role_id: Set(role.id),
                permission: Set((*permission).into()),
            }
            .insert(&self.state.db)
            .await
            .unwrap();
        }
        user_roles::ActiveModel {
            user_id: Set(user_id),
            role_id: Set(role.id),
            all_libraries: Set(true),
        }
        .insert(&self.state.db)
        .await
        .unwrap();

        cookie
    }

    /// A member whose role is scoped to the given libraries; `None` means all
    /// libraries.
    async fn member_scoped(
        &self,
        email: &str,
        permissions: &[&str],
        library_ids: Option<&[i32]>,
    ) -> String {
        let prefix = &self.state.base_path;
        self.state.policy.admit_next_signup();
        let sign_up = json!({ "email": email, "password": PASSWORD, "name": "Seerr" });
        let reply = self
            .send(
                Request::post(format!("{prefix}/api/auth/sign-up/email"))
                    .header(header::HOST, HOST)
                    .header(header::ORIGIN, ORIGIN)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(sign_up.to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let user_id = reply.body["user"]["id"].as_str().unwrap().to_owned();
        let cookie = reply.session_pair();

        let role = roles::ActiveModel {
            name: Set(format!("Role of {email}")),
            is_owner: Set(false),
            created_at: Set(db::now()),
            updated_at: Set(db::now()),
            ..Default::default()
        }
        .insert(&self.state.db)
        .await
        .unwrap();
        for permission in permissions {
            role_permissions::ActiveModel {
                role_id: Set(role.id),
                permission: Set((*permission).into()),
            }
            .insert(&self.state.db)
            .await
            .unwrap();
        }
        user_roles::ActiveModel {
            user_id: Set(user_id.clone()),
            role_id: Set(role.id),
            all_libraries: Set(library_ids.is_none()),
        }
        .insert(&self.state.db)
        .await
        .unwrap();
        if let Some(library_ids) = library_ids {
            for library_id in library_ids {
                user_role_libraries::ActiveModel {
                    user_id: Set(user_id.clone()),
                    role_id: Set(role.id),
                    library_id: Set(*library_id),
                }
                .insert(&self.state.db)
                .await
                .unwrap();
            }
        }

        cookie
    }

    async fn create_key(&self, cookie: &str, permissions: &[&str]) -> String {
        let prefix = &self.state.base_path;
        let body = json!({ "name": "Seerr", "permissions": permissions });
        let reply = self
            .send(
                Request::post(format!("{prefix}/api/v1/me/api-keys"))
                    .header(header::HOST, HOST)
                    .header(header::COOKIE, cookie)
                    .header(header::ORIGIN, ORIGIN)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
        reply.body["key"].as_str().unwrap().to_owned()
    }
}

fn req(method: Method, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
}

#[test]
fn sonarr_routes_declare_expected_permissions() {
    let routes = sonarr_routes();
    assert_eq!(routes.verify(), Ok(()));
    assert_eq!(
        routes.entries(),
        &[
            CompatRouteEntry {
                method: Method::GET,
                path: "/sonarr/api/v3/system/status".to_owned(),
                permission: Some(Permission::SettingsRead),
            },
            CompatRouteEntry {
                method: Method::GET,
                path: "/sonarr/api/v3/qualityprofile".to_owned(),
                permission: Some(Permission::ProfilesRead),
            },
            CompatRouteEntry {
                method: Method::GET,
                path: "/sonarr/api/v3/rootfolder".to_owned(),
                permission: Some(Permission::SettingsRead),
            },
            CompatRouteEntry {
                method: Method::GET,
                path: "/sonarr/api/v3/languageprofile".to_owned(),
                permission: Some(Permission::ProfilesRead),
            },
            CompatRouteEntry {
                method: Method::GET,
                path: "/sonarr/api/v3/series".to_owned(),
                permission: Some(Permission::LibraryRead),
            },
            CompatRouteEntry {
                method: Method::GET,
                path: "/sonarr/api/v3/series/lookup".to_owned(),
                permission: Some(Permission::LibraryRead),
            },
            CompatRouteEntry {
                method: Method::GET,
                path: "/sonarr/api/v3/series/{id}".to_owned(),
                permission: Some(Permission::LibraryRead),
            },
            CompatRouteEntry {
                method: Method::GET,
                path: "/sonarr/api/v3/episode".to_owned(),
                permission: Some(Permission::LibraryRead),
            },
            CompatRouteEntry {
                method: Method::PUT,
                path: "/sonarr/api/v3/episode/monitor".to_owned(),
                permission: Some(Permission::LibraryEdit),
            },
            CompatRouteEntry {
                method: Method::GET,
                path: "/sonarr/api/v3/tag".to_owned(),
                permission: Some(Permission::LibraryRead),
            },
            CompatRouteEntry {
                method: Method::POST,
                path: "/sonarr/api/v3/tag".to_owned(),
                permission: Some(Permission::LibraryEdit),
            },
            CompatRouteEntry {
                method: Method::PUT,
                path: "/sonarr/api/v3/tag/{id}".to_owned(),
                permission: Some(Permission::LibraryEdit),
            },
        ]
    );
}

#[tokio::test]
async fn sonarr_requires_setup_before_owner_exists() {
    let app = App::uninitialized(None).await;
    for path in [
        "/sonarr/api/v3/system/status",
        "/Sonarr/API/v3/System/Status",
    ] {
        let reply = app
            .send(req(Method::GET, path).body(Body::empty()).unwrap())
            .await;
        assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(reply.body["code"], "setup_required", "{path}");
    }
}

#[tokio::test]
async fn unauthenticated_and_cookie_requests_return_401_without_warn() {
    let app = App::new().await;

    for uri in [
        "/sonarr/api/v3/system/status",
        "/sonarr/api/v3/anything",
        "/Sonarr/API/v3/QualityProfile",
    ] {
        let capture = CaptureLayer::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        let reply = app
            .send(req(Method::GET, uri).body(Body::empty()).unwrap())
            .with_subscriber(subscriber)
            .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(reply.content_type, "application/json", "{uri}");
        assert_eq!(reply.body, json!({ "error": "Unauthorized" }), "{uri}");
        assert!(capture.warns().is_empty(), "{uri}: {:?}", capture.warns());
    }

    // Session cookie alone is never accepted on the compat API.
    let cookie_reply = app
        .send(
            req(Method::GET, "/sonarr/api/v3/system/status")
                .header(header::COOKIE, &app.owner)
                .header(header::ORIGIN, ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(cookie_reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(cookie_reply.body, json!({ "error": "Unauthorized" }));
}

#[tokio::test]
async fn unimplemented_sonarr_routes_return_501_and_log_one_warn() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &[]).await;

    let capture = CaptureLayer::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let reply = app
        .send(
            req(Method::GET, &format!("/Sonarr/API/v3/Queue?apikey={key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .with_subscriber(subscriber)
        .await;
    assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(reply.content_type, "application/json");
    assert_eq!(
        reply.body,
        json!({
            "message": "Not implemented",
            "method": "GET",
            "path": "/Sonarr/API/v3/Queue",
        })
    );
    let warns = capture.warns();
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert_eq!(
        warns[0].fields.get("method").map(String::as_str),
        Some("GET")
    );
    assert_eq!(
        warns[0].fields.get("path").map(String::as_str),
        Some("/Sonarr/API/v3/Queue")
    );
    assert!(
        !warns[0].fields.values().any(|value| value.contains(&key)),
        "API key leaked into warn log: {:?}",
        warns[0].fields
    );
}

#[tokio::test]
async fn every_implemented_route_enforces_its_permission() {
    let app = App::new().await;
    let cookie = app.member("limited@example.com", &[]).await;
    let empty_key = app.create_key(&cookie, &[]).await;

    let cases = [
        (Method::GET, "/sonarr/api/v3/system/status", "settings:read"),
        (
            Method::GET,
            "/sonarr/api/v3/qualityProfile",
            "profiles:read",
        ),
        (Method::GET, "/sonarr/api/v3/rootfolder", "settings:read"),
        (
            Method::GET,
            "/sonarr/api/v3/languageprofile",
            "profiles:read",
        ),
        (Method::GET, "/sonarr/api/v3/tag", "library:read"),
        (Method::GET, "/sonarr/api/v3/series", "library:read"),
        (Method::GET, "/sonarr/api/v3/series/1", "library:read"),
        (Method::GET, "/sonarr/api/v3/series/lookup", "library:read"),
        (
            Method::GET,
            "/sonarr/api/v3/episode?seriesId=1",
            "library:read",
        ),
        (
            Method::PUT,
            "/sonarr/api/v3/episode/monitor",
            "library:edit",
        ),
        (Method::POST, "/sonarr/api/v3/tag", "library:edit"),
        (Method::PUT, "/sonarr/api/v3/tag/1", "library:edit"),
    ];

    for (method, path, expected_perm) in cases {
        let reply = app
            .send(
                req(method.clone(), path)
                    .header("X-Api-Key", &empty_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(reply.content_type, "application/json", "{method} {path}");
        assert_eq!(reply.body["message"], "Forbidden", "{method} {path}");
        assert_eq!(
            reply.body["requiredPermission"], expected_perm,
            "{method} {path}"
        );
    }
}

#[tokio::test]
async fn system_status_reports_sonarr_version_and_respects_base_path() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["settings:read"]).await;

    for path in [
        "/sonarr/api/v3/system/status",
        "/Sonarr/api/v3/System/Status",
    ] {
        let reply = app
            .send(
                req(Method::GET, path)
                    .header("X-Api-Key", &key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.body);
        let status: SystemResource = serde_json::from_value(reply.body).unwrap();
        assert_eq!(status.app_name, "Sonarr");
        assert_eq!(status.instance_name, "Sonarr");
        assert_eq!(status.version, SONARR_VERSION);
        assert_eq!(status.url_base, "/sonarr");
        assert!(!status.sqlite_version.is_empty());
        assert_eq!(status.sqlite_version, status.database_version);
        assert!(chrono::DateTime::parse_from_rfc3339(&status.start_time.0).is_ok());
    }

    // Sub-path hosting prefixes `urlBase` cleanly.
    let sub = App::with_base_url(Some("http://unseenarr.test/unseenarr")).await;
    let sub_key = sub.create_key(&sub.owner, &["settings:read"]).await;
    let reply = sub
        .send(
            req(Method::GET, "/unseenarr/sonarr/api/v3/system/status")
                .header("X-Api-Key", &sub_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let status: SystemResource = serde_json::from_value(reply.body).unwrap();
    assert_eq!(status.url_base, "/unseenarr/sonarr");
}

#[tokio::test]
async fn quality_profiles_translate_tv_quality_ids_and_match_case_insensitively() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["profiles:read"]).await;

    // Create a profile using qualities whose Sonarr ids differ from Unseenarr's.
    let custom = db::profiles::create(
        &app.state.db,
        db::profiles::Values {
            name: "Remux TV".to_owned(),
            upgrade_allowed: true,
            cutoff_quality_id: CoreQuality::Remux1080p.id(), // Unseenarr 30 -> Sonarr 20
            items: json!([
                { "type": "quality", "qualityId": CoreQuality::Bluray480p.id(), "allowed": false },
                { "type": "quality", "qualityId": CoreQuality::Remux1080p.id(), "allowed": true }
            ]),
            language: "en".to_owned(),
            min_format_score: 0,
            cutoff_format_score: 0,
            min_upgrade_format_score: 1,
            format_scores: Vec::new(),
        },
    )
    .await
    .unwrap();

    // Seerr specifically requests `/qualityProfile` with a capital P.
    let reply = app
        .send(
            req(Method::GET, "/sonarr/api/v3/qualityProfile")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let profiles: Vec<QualityProfileResource> = serde_json::from_value(reply.body).unwrap();
    let found = profiles
        .iter()
        .find(|p| p.id == custom.id)
        .expect("custom profile present");
    assert_eq!(found.name, "Remux TV");
    assert_eq!(found.cutoff, 20);
    assert_eq!(found.items.len(), 2);
    assert_eq!(found.items[0].quality.as_ref().unwrap().id, 13);
    assert_eq!(found.items[1].quality.as_ref().unwrap().id, 20);
    assert_eq!(
        found.items[1].quality.as_ref().unwrap().name,
        "Bluray-1080p Remux"
    );
}

#[tokio::test]
async fn root_folders_report_accessibility_space_and_unmapped_subfolders() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["settings:read"]).await;

    let tv_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(tv_dir.path().join("The Expanse")).unwrap();
    let missing_path = tv_dir.path().join("does-not-exist");

    let live_folder =
        db::root_folders::create(&app.state.db, tv_dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
    let missing_folder =
        db::root_folders::create(&app.state.db, missing_path.to_string_lossy().into_owned())
            .await
            .unwrap();

    let reply = app
        .send(
            req(Method::GET, "/sonarr/api/v3/rootfolder")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let folders: Vec<RootFolderResource> = serde_json::from_value(reply.body).unwrap();
    assert_eq!(folders.len(), 2);

    let live = folders.iter().find(|f| f.id == live_folder.id).unwrap();
    assert!(live.accessible);
    assert!(live.free_space.is_some());
    assert!(live.total_space.is_some());
    assert_eq!(live.unmapped_folders.len(), 1);
    assert_eq!(live.unmapped_folders[0].name, "The Expanse");

    let missing = folders.iter().find(|f| f.id == missing_folder.id).unwrap();
    assert!(!missing.accessible);
    assert_eq!(missing.free_space, None);
    assert_eq!(missing.total_space, None);
    assert!(missing.unmapped_folders.is_empty());
}

#[tokio::test]
async fn language_profiles_returns_empty_list() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["profiles:read"]).await;

    let reply = app
        .send(
            req(Method::GET, "/sonarr/api/v3/languageprofile")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let profiles: Vec<LanguageProfileResource> = serde_json::from_value(reply.body).unwrap();
    assert!(profiles.is_empty());
}

#[tokio::test]
async fn tags_support_list_create_update_and_sonarr_errors() {
    let app = App::new().await;
    let key = app
        .create_key(&app.owner, &["library:read", "library:edit"])
        .await;

    // Initially empty.
    let initial = app
        .send(
            req(Method::GET, "/sonarr/api/v3/tag")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(initial.status, StatusCode::OK);
    assert_eq!(initial.body, json!([]));

    // Create a tag (uppercase normalized to lowercase).
    let created = app
        .send(
            req(Method::POST, "/sonarr/api/v3/tag")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "label": "1-Alice" }).to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let tag: TagResource = serde_json::from_value(created.body).unwrap();
    assert!(tag.id > 0);
    assert_eq!(tag.label, "1-alice");

    // Duplicate label returns 400 validation array.
    let dup = app
        .send(
            req(Method::POST, "/sonarr/api/v3/tag")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "label": "1-alice" }).to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(dup.status, StatusCode::BAD_REQUEST, "{}", dup.body);
    assert_eq!(dup.body[0]["propertyName"], "label");

    // Empty or invalid label returns 400 validation array.
    for bad_label in ["", "   ", "has space!"] {
        let invalid = app
            .send(
                req(Method::POST, "/sonarr/api/v3/tag")
                    .header("X-Api-Key", &key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "label": bad_label }).to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(invalid.status, StatusCode::BAD_REQUEST, "{bad_label}");
        assert_eq!(invalid.body[0]["propertyName"], "label");
    }

    // Rename tag via PUT /tag/{id}.
    let renamed = app
        .send(
            req(Method::PUT, &format!("/sonarr/api/v3/tag/{}", tag.id))
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "id": tag.id, "label": "1-alice-updated" }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    let updated: TagResource = serde_json::from_value(renamed.body).unwrap();
    assert_eq!(updated.id, tag.id);
    assert_eq!(updated.label, "1-alice-updated");

    // Unknown tag id returns 404 Sonarr JSON.
    let missing = app
        .send(
            req(Method::PUT, "/sonarr/api/v3/tag/99999")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "label": "other" }).to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.body, json!({ "message": "NotFound" }));

    // Malformed JSON or invalid path id returns 400 JSON.
    let bad_json = app
        .send(
            req(Method::POST, "/sonarr/api/v3/tag")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{not-json"))
                .unwrap(),
        )
        .await;
    assert_eq!(bad_json.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_json.content_type, "application/json");

    let bad_id = app
        .send(
            req(Method::PUT, "/sonarr/api/v3/tag/not-a-number")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "label": "ok" }).to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(bad_id.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_id.content_type, "application/json");

    // List reflects the renamed tag.
    let listed = app
        .send(
            req(Method::GET, "/sonarr/api/v3/tag")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK);
    let tags: Vec<TagResource> = serde_json::from_value(listed.body).unwrap();
    assert_eq!(tags, vec![updated]);
}

#[tokio::test]
async fn seerr_sonarr_connection_test_sequence_passes() {
    let app = App::new().await;
    let tv_dir = tempfile::tempdir().unwrap();
    db::root_folders::create(&app.state.db, tv_dir.path().to_string_lossy().into_owned())
        .await
        .unwrap();
    db::tags::create(&app.state.db, "seerr".to_owned())
        .await
        .unwrap();

    // Key scoped to the permissions Seerr needs for its Sonarr connection test.
    let key = app
        .create_key(
            &app.owner,
            &["settings:read", "profiles:read", "library:read"],
        )
        .await;

    // 1. GET /system/status
    let status = app
        .send(
            req(Method::GET, "/sonarr/api/v3/system/status")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);
    let status_dto: SystemResource = serde_json::from_value(status.body).unwrap();
    assert_eq!(status_dto.app_name, "Sonarr");
    assert_eq!(status_dto.version, SONARR_VERSION);

    // 2. GET /qualityProfile (Seerr's exact casing)
    let profiles = app
        .send(
            req(Method::GET, "/sonarr/api/v3/qualityProfile")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(profiles.status, StatusCode::OK, "{}", profiles.body);
    let profile_dtos: Vec<QualityProfileResource> = serde_json::from_value(profiles.body).unwrap();
    assert!(!profile_dtos.is_empty());

    // 3. GET /rootfolder
    let folders = app
        .send(
            req(Method::GET, "/sonarr/api/v3/rootfolder")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(folders.status, StatusCode::OK, "{}", folders.body);
    let folder_dtos: Vec<RootFolderResource> = serde_json::from_value(folders.body).unwrap();
    assert_eq!(folder_dtos.len(), 1);
    assert!(folder_dtos[0].accessible);

    // 4. GET /languageprofile
    let lang_profiles = app
        .send(
            req(Method::GET, "/sonarr/api/v3/languageprofile")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        lang_profiles.status,
        StatusCode::OK,
        "{}",
        lang_profiles.body
    );
    let lang_dtos: Vec<LanguageProfileResource> =
        serde_json::from_value(lang_profiles.body).unwrap();
    assert!(lang_dtos.is_empty());

    // 5. GET /tag
    let tags = app
        .send(
            req(Method::GET, "/sonarr/api/v3/tag")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(tags.status, StatusCode::OK, "{}", tags.body);
    let tag_dtos: Vec<TagResource> = serde_json::from_value(tags.body).unwrap();
    assert_eq!(tag_dtos.len(), 1);
    assert_eq!(tag_dtos[0].label, "seerr");
}

async fn add_show(app: &App, root_folder_id: i32, tvdb_id: i32, title: &str) -> i32 {
    shows::ActiveModel {
        tvdb_id: Set(Some(tvdb_id)),
        metadata_provider: Set("tvdb".into()),
        title: Set(title.into()),
        sort_title: Set(title.into()),
        year: Set(2020),
        status: Set("continuing".into()),
        series_type: Set("standard".into()),
        folder_name: Set(title.into()),
        monitored: Set(true),
        root_folder_id: Set(root_folder_id),
        added_at: Set(db::now()),
        updated_at: Set(db::now()),
        ..Default::default()
    }
    .insert(&app.state.db)
    .await
    .unwrap()
    .id
}

async fn add_season(app: &App, show_id: i32, number: i32) {
    seasons::ActiveModel {
        show_id: Set(show_id),
        season_number: Set(number),
        monitored: Set(true),
    }
    .insert(&app.state.db)
    .await
    .unwrap();
}

async fn add_episode(app: &App, show_id: i32, season: i32, number: i32, air_utc: &str) -> i32 {
    episodes::ActiveModel {
        show_id: Set(show_id),
        season_number: Set(season),
        episode_number: Set(number),
        title: Set(format!("Episode {number}")),
        air_date_utc: Set(Some(air_utc.into())),
        monitored: Set(true),
        added_at: Set(db::now()),
        updated_at: Set(db::now()),
        ..Default::default()
    }
    .insert(&app.state.db)
    .await
    .unwrap()
    .id
}

async fn add_episode_file(app: &App, show_id: i32, size: i64, group: &str, episode_id: i32) {
    let file = episode_files::ActiveModel {
        show_id: Set(show_id),
        relative_path: Set("Season 01/Episode 01.mkv".into()),
        size: Set(size),
        quality_id: Set(1),
        release_group: Set(Some(group.into())),
        added_at: Set(db::now()),
        ..Default::default()
    }
    .insert(&app.state.db)
    .await
    .unwrap();
    episode_file_episodes::ActiveModel {
        episode_file_id: Set(file.id),
        episode_id: Set(episode_id),
    }
    .insert(&app.state.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn series_list_maps_shows_with_season_statistics() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["library:read"]).await;

    let tv_dir = tempfile::tempdir().unwrap();
    let root =
        db::root_folders::create(&app.state.db, tv_dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

    let show_id = add_show(&app, root.id, 81189, "Breaking Bad").await;
    add_season(&app, show_id, 0).await;
    add_season(&app, show_id, 1).await;
    let first = add_episode(&app, show_id, 1, 1, "2008-01-20T02:00:00Z").await;
    add_episode(&app, show_id, 1, 2, "2099-01-01T00:00:00Z").await;
    add_episode_file(&app, show_id, 1_000, "GROUP", first).await;

    let reply = app
        .send(
            req(Method::GET, "/sonarr/api/v3/series")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let series: Vec<SeriesResource> = serde_json::from_value(reply.body).unwrap();
    assert_eq!(series.len(), 1);
    let show = &series[0];
    assert_eq!(show.id, show_id);
    assert_eq!(show.tvdb_id, 81189);
    assert_eq!(show.title, "Breaking Bad");
    assert_eq!(
        show.root_folder_path,
        tv_dir.path().to_string_lossy().as_ref()
    );
    assert_eq!(show.folder, "Breaking Bad");
    assert_eq!(show.quality_profile_id, 0);
    assert!(show.tags.is_empty());

    assert_eq!(show.seasons.len(), 2);
    assert_eq!(show.seasons[0].season_number, 0);
    let season = &show.seasons[1];
    assert_eq!(season.season_number, 1);
    assert_eq!(season.statistics.episode_file_count, 1);
    assert_eq!(season.statistics.episode_count, 2);
    assert_eq!(season.statistics.total_episode_count, 2);
    assert_eq!(season.statistics.size_on_disk, 1_000);
    assert_eq!(season.statistics.release_groups, ["GROUP"]);
    assert_eq!(season.statistics.percent_of_episodes, 50.0);
    assert_eq!(
        season
            .statistics
            .previous_airing
            .as_ref()
            .map(|t| t.0.as_str()),
        Some("2008-01-20T02:00:00Z")
    );
    assert_eq!(
        season.statistics.next_airing.as_ref().map(|t| t.0.as_str()),
        Some("2099-01-01T00:00:00Z")
    );

    assert_eq!(show.statistics.season_count, 1);
    assert_eq!(show.statistics.episode_file_count, 1);
    assert_eq!(show.statistics.total_episode_count, 2);
    assert_eq!(show.statistics.size_on_disk, 1_000);
    assert_eq!(show.statistics.percent_of_episodes, 50.0);
}

#[tokio::test]
async fn series_filter_by_tvdb_id_and_lookup_stays_unimplemented() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["library:read"]).await;

    let tv_dir = tempfile::tempdir().unwrap();
    let root =
        db::root_folders::create(&app.state.db, tv_dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
    let wanted = add_show(&app, root.id, 81189, "Breaking Bad").await;
    add_show(&app, root.id, 121361, "Game of Thrones").await;

    let reply = app
        .send(
            req(Method::GET, "/sonarr/api/v3/series?tvdbId=81189")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let series: Vec<SeriesResource> = serde_json::from_value(reply.body).unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].id, wanted);

    let miss = app
        .send(
            req(Method::GET, "/sonarr/api/v3/series?tvdbId=1")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(miss.status, StatusCode::OK, "{}", miss.body);
    assert_eq!(miss.body, json!([]));

    let lookup = app
        .send(
            req(Method::GET, "/sonarr/api/v3/series/lookup?term=tvdb:81189")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        lookup.status,
        StatusCode::NOT_IMPLEMENTED,
        "{}",
        lookup.body
    );
}

#[tokio::test]
async fn series_get_returns_one_show_and_404s_for_unknown_ids() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["library:read"]).await;

    let tv_dir = tempfile::tempdir().unwrap();
    let root =
        db::root_folders::create(&app.state.db, tv_dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
    let show_id = add_show(&app, root.id, 81189, "Breaking Bad").await;

    let reply = app
        .send(
            req(Method::GET, &format!("/sonarr/api/v3/series/{show_id}"))
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let show: SeriesResource = serde_json::from_value(reply.body).unwrap();
    assert_eq!(show.id, show_id);
    assert_eq!(show.tvdb_id, 81189);

    let missing = app
        .send(
            req(Method::GET, "/sonarr/api/v3/series/999999")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);

    let invalid = app
        .send(
            req(Method::GET, "/sonarr/api/v3/series/abc")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST, "{}", invalid.body);
}

#[tokio::test]
async fn series_list_is_empty_without_shows() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["library:read"]).await;

    let reply = app
        .send(
            req(Method::GET, "/sonarr/api/v3/series")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body, json!([]));
}

#[tokio::test]
async fn episode_list_maps_episodes_with_sonarr_defaults() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["library:read"]).await;

    let tv_dir = tempfile::tempdir().unwrap();
    let root =
        db::root_folders::create(&app.state.db, tv_dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
    let show_id = add_show(&app, root.id, 81189, "Breaking Bad").await;
    add_season(&app, show_id, 1).await;
    let first = add_episode(&app, show_id, 1, 1, "2008-01-20T02:00:00Z").await;
    add_episode(&app, show_id, 1, 2, "2099-01-01T00:00:00Z").await;
    add_episode_file(&app, show_id, 1_000, "GROUP", first).await;

    let reply = app
        .send(
            req(
                Method::GET,
                &format!("/sonarr/api/v3/episode?seriesId={show_id}"),
            )
            .header("X-Api-Key", &key)
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let episodes: Vec<EpisodeResource> = serde_json::from_value(reply.body).unwrap();
    assert_eq!(episodes.len(), 2);
    let watched = &episodes[0];
    assert_eq!(watched.series_id, show_id);
    assert_eq!(watched.season_number, 1);
    assert_eq!(watched.episode_number, 1);
    assert_eq!(watched.title, "Episode 1");
    assert!(watched.monitored);
    assert!(watched.has_file);
    assert!(watched.episode_file_id > 0);
    assert!(watched.episode_file.is_none());
    assert!(watched.series.is_none());
    assert!(!episodes[1].has_file);

    let expanded = app
        .send(
            req(
                Method::GET,
                &format!(
                    "/sonarr/api/v3/episode?seriesId={show_id}&includeEpisodeFile=true&includeSeries=true"
                ),
            )
            .header("X-Api-Key", &key)
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(expanded.status, StatusCode::OK, "{}", expanded.body);
    let episodes: Vec<EpisodeResource> = serde_json::from_value(expanded.body).unwrap();
    let file = episodes[0].episode_file.as_ref().expect("episode file");
    assert_eq!(file.series_id, show_id);
    assert_eq!(file.season_number, 1);
    assert_eq!(file.relative_path, "Season 01/Episode 01.mkv");
    assert_eq!(file.release_type, ReleaseType::SingleEpisode);
    assert_eq!(file.release_group, "GROUP");
    assert!(file.path.ends_with("Breaking Bad/Season 01/Episode 01.mkv"));
    let series = episodes[0].series.as_ref().expect("series");
    assert_eq!(series.id, show_id);
    assert_eq!(series.title, "Breaking Bad");
}

#[tokio::test]
async fn episode_list_filters_by_season_and_validates_series_id() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["library:read"]).await;

    let tv_dir = tempfile::tempdir().unwrap();
    let root =
        db::root_folders::create(&app.state.db, tv_dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
    let show_id = add_show(&app, root.id, 81189, "Breaking Bad").await;
    for season in 0..=2 {
        add_season(&app, show_id, season).await;
    }
    add_episode(&app, show_id, 0, 1, "2008-01-20T02:00:00Z").await;
    add_episode(&app, show_id, 1, 1, "2008-01-27T02:00:00Z").await;
    add_episode(&app, show_id, 2, 1, "2009-03-08T02:00:00Z").await;

    let reply = app
        .send(
            req(
                Method::GET,
                &format!("/sonarr/api/v3/episode?seriesId={show_id}&seasonNumber=1"),
            )
            .header("X-Api-Key", &key)
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let episodes: Vec<EpisodeResource> = serde_json::from_value(reply.body).unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].season_number, 1);

    for uri in [
        "/sonarr/api/v3/episode",
        "/sonarr/api/v3/episode?seriesId=",
        "/sonarr/api/v3/episode?seriesId=0",
        "/sonarr/api/v3/episode?seriesId=abc",
    ] {
        let reply = app
            .send(
                req(Method::GET, uri)
                    .header("X-Api-Key", &key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{uri}");
    }

    let bad_season = app
        .send(
            req(
                Method::GET,
                &format!("/sonarr/api/v3/episode?seriesId={show_id}&seasonNumber=abc"),
            )
            .header("X-Api-Key", &key)
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(bad_season.status, StatusCode::BAD_REQUEST);

    let unknown = app
        .send(
            req(Method::GET, "/sonarr/api/v3/episode?seriesId=999999")
                .header("X-Api-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn episode_monitor_updates_dedupes_and_rejects_unknown_ids() {
    let app = App::new().await;
    let key = app.create_key(&app.owner, &["library:edit"]).await;

    let tv_dir = tempfile::tempdir().unwrap();
    let root =
        db::root_folders::create(&app.state.db, tv_dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
    let show_id = add_show(&app, root.id, 81189, "Breaking Bad").await;
    add_season(&app, show_id, 1).await;
    let first = add_episode(&app, show_id, 1, 1, "2008-01-20T02:00:00Z").await;
    let second = add_episode(&app, show_id, 1, 2, "2008-01-27T02:00:00Z").await;

    let reply = app
        .send(
            req(Method::PUT, "/sonarr/api/v3/episode/monitor")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "episodeIds": [first, first, second], "monitored": false }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let episodes = db::shows::episodes_of(&app.state.db, show_id)
        .await
        .unwrap();
    assert_eq!(episodes.len(), 2);
    assert!(episodes.iter().all(|episode| !episode.monitored));

    let repeated = app
        .send(
            req(Method::PUT, "/sonarr/api/v3/episode/monitor")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "episodeIds": [first, second], "monitored": false }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(repeated.status, StatusCode::OK, "{}", repeated.body);

    let missing = app
        .send(
            req(Method::PUT, "/sonarr/api/v3/episode/monitor")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "episodeIds": [999999], "monitored": true }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);

    for body in [
        json!({ "episodeIds": [], "monitored": false }),
        json!({ "episodeIds": [first] }),
        json!({ "episodeIds": [0], "monitored": true }),
    ] {
        let reply = app
            .send(
                req(Method::PUT, "/sonarr/api/v3/episode/monitor")
                    .header("X-Api-Key", &key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
    }

    let malformed = app
        .send(
            req(Method::PUT, "/sonarr/api/v3/episode/monitor")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{not-json"))
                .unwrap(),
        )
        .await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn episode_endpoints_honour_library_scope() {
    let app = App::new().await;

    let allowed_dir = tempfile::tempdir().unwrap();
    let allowed_root = db::root_folders::create(
        &app.state.db,
        allowed_dir.path().to_string_lossy().into_owned(),
    )
    .await
    .unwrap();
    let allowed_show = add_show(&app, allowed_root.id, 81189, "Breaking Bad").await;
    add_season(&app, allowed_show, 1).await;
    let allowed_episode = add_episode(&app, allowed_show, 1, 1, "2008-01-20T02:00:00Z").await;

    let other_library = db::libraries::create(&app.state.db, "Other".to_owned())
        .await
        .unwrap()
        .library
        .id;
    let other_dir = tempfile::tempdir().unwrap();
    let other_root = db::root_folders::create_in_library(
        &app.state.db,
        other_dir.path().to_string_lossy().into_owned(),
        other_library,
    )
    .await
    .unwrap();
    let other_show = add_show(&app, other_root.id, 121361, "Game of Thrones").await;
    add_season(&app, other_show, 1).await;
    let other_episode = add_episode(&app, other_show, 1, 1, "2011-04-17T01:00:00Z").await;

    let cookie = app
        .member_scoped(
            "scoped@example.com",
            &["library:read", "library:edit"],
            Some(&[allowed_root.library_id]),
        )
        .await;
    let key = app
        .create_key(&cookie, &["library:read", "library:edit"])
        .await;

    let visible = app
        .send(
            req(
                Method::GET,
                &format!("/sonarr/api/v3/episode?seriesId={allowed_show}"),
            )
            .header("X-Api-Key", &key)
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(visible.status, StatusCode::OK, "{}", visible.body);

    let hidden = app
        .send(
            req(
                Method::GET,
                &format!("/sonarr/api/v3/episode?seriesId={other_show}"),
            )
            .header("X-Api-Key", &key)
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{}", hidden.body);

    let hidden_monitor = app
        .send(
            req(Method::PUT, "/sonarr/api/v3/episode/monitor")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "episodeIds": [other_episode], "monitored": false }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(
        hidden_monitor.status,
        StatusCode::NOT_FOUND,
        "{}",
        hidden_monitor.body
    );
    let untouched = db::shows::episodes_of(&app.state.db, other_show)
        .await
        .unwrap();
    assert!(untouched.iter().all(|episode| episode.monitored));

    let allowed_monitor = app
        .send(
            req(Method::PUT, "/sonarr/api/v3/episode/monitor")
                .header("X-Api-Key", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "episodeIds": [allowed_episode], "monitored": false }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(
        allowed_monitor.status,
        StatusCode::OK,
        "{}",
        allowed_monitor.body
    );
}
