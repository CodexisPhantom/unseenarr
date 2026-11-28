use api::access::{
    Access, ApiRouter, Caller, LibraryRead, Public, Require, RouteEntry, X_PERMISSION,
};
use api::{AppState, router};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Json;
use axum::routing::get;
use better_auth::integrations::axum::AxumIntegration;
use better_auth::seaorm::sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use db::entities::{role_permissions, roles, user_permissions, user_roles};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use unseenarr_core::permission::Permission;
use utoipa_axum::routes;

const HOST: &str = "unseenarr.test";
const PASSWORD: &str = "correct horse";

struct TestApp {
    state: AppState,
    origin: String,
    _dir: TempDir,
}

struct Reply {
    status: StatusCode,
    content_type: String,
    cookies: Vec<String>,
    body: Value,
}

impl Reply {
    fn session_pair(&self) -> &str {
        self.cookies
            .iter()
            .find(|cookie| cookie.contains("session_token="))
            .expect("a session cookie")
            .split(';')
            .next()
            .unwrap()
    }
}

impl TestApp {
    async fn new(base_url: Option<&str>) -> Self {
        let (db, dir) = db::testing::temp_db().await;
        let state = AppState::new(db, "test secret", base_url).await.unwrap();
        Self {
            state,
            origin: base_url.unwrap_or("http://unseenarr.test").to_owned(),
            _dir: dir,
        }
    }

    fn mount(&self, api_router: ApiRouter) -> Router {
        let auth = self.state.auth.clone().axum_router_with_state();
        api_router
            .into_router(&self.state)
            .nest("/api/auth", auth)
            .with_state(self.state.clone())
    }

    async fn send(&self, router: &Router, request: Request<Body>) -> Reply {
        let response = router.clone().oneshot(request).await.unwrap();
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

    fn request(&self, method: &str, path: &str, cookie: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, HOST);
        if let Some(cookie) = cookie {
            builder = builder
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, &self.origin);
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn post_json(
        &self,
        router: &Router,
        path: &str,
        body: Value,
        cookie: Option<&str>,
    ) -> Reply {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            builder = builder
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, &self.origin);
        }
        let request = builder.body(Body::from(body.to_string())).unwrap();
        self.send(router, request).await
    }

    async fn create_user_and_sign_in(&self, router: &Router, email: &str) -> (String, String) {
        self.state.policy.admit_next_signup();
        let sign_up = self
            .post_json(
                router,
                "/api/auth/sign-up/email",
                json!({ "name": "Test User", "email": email, "password": PASSWORD }),
                None,
            )
            .await;
        assert_eq!(sign_up.status, StatusCode::OK, "{}", sign_up.body);
        let user_id = sign_up.body["user"]["id"].as_str().unwrap().to_owned();

        let sign_in = self
            .post_json(
                router,
                "/api/auth/sign-in/email",
                json!({ "email": email, "password": PASSWORD }),
                None,
            )
            .await;
        assert_eq!(sign_in.status, StatusCode::OK, "{}", sign_in.body);
        let cookie = sign_in.session_pair().to_owned();
        (user_id, cookie)
    }

    async fn create_role_with_permissions(&self, name: &str, perms: &[&str]) -> i32 {
        let role = roles::ActiveModel {
            name: Set(name.into()),
            is_owner: Set(false),
            created_at: Set(db::now()),
            updated_at: Set(db::now()),
            ..Default::default()
        }
        .insert(&self.state.db)
        .await
        .unwrap();
        for perm in perms {
            role_permissions::ActiveModel {
                role_id: Set(role.id),
                permission: Set((*perm).into()),
            }
            .insert(&self.state.db)
            .await
            .unwrap();
        }
        role.id
    }

    async fn assign_role(&self, user_id: &str, role_id: i32) {
        user_roles::ActiveModel {
            user_id: Set(user_id.into()),
            role_id: Set(role_id),
            all_libraries: Set(true),
        }
        .insert(&self.state.db)
        .await
        .unwrap();
    }

    async fn grant_direct(&self, user_id: &str, perm: &str) {
        user_permissions::ActiveModel {
            user_id: Set(user_id.into()),
            permission: Set(perm.into()),
            all_libraries: Set(true),
        }
        .insert(&self.state.db)
        .await
        .unwrap();
    }
}

#[utoipa::path(get, path = "/api/v1/test/public", responses((status = 200)))]
async fn test_public(_: Public) -> Json<Value> {
    Json(json!({ "ok": true }))
}

#[utoipa::path(get, path = "/api/v1/test/me", responses((status = 200)))]
async fn test_me(caller: Caller) -> Json<Value> {
    Json(json!({
        "userId": caller.user_id(),
        "isOwner": caller.is_owner(),
        "permissionCount": caller.permissions().iter().count(),
    }))
}

#[utoipa::path(get, path = "/api/v1/test/items", responses((status = 200)))]
async fn test_list_items(
    caller: Require<LibraryRead>,
    State(_state): State<AppState>,
) -> Json<Value> {
    let caller = caller.into_caller();
    Json(json!({
        "userId": caller.user_id(),
        "isOwner": caller.is_owner(),
    }))
}

#[utoipa::path(post, path = "/api/v1/test/items", responses((status = 201)))]
async fn test_create_item(caller: Caller) -> (StatusCode, Json<Value>) {
    (
        StatusCode::CREATED,
        Json(json!({ "createdBy": caller.user_id() })),
    )
}

fn sample_api_router() -> ApiRouter {
    ApiRouter::new()
        .public(routes!(test_public))
        .authenticated(routes!(test_me))
        .require(Permission::LibraryRead, routes!(test_list_items))
        .require(Permission::LibraryAdd, routes!(test_create_item))
}

fn roles_manage(method: Method, path: &str) -> RouteEntry {
    RouteEntry {
        method,
        path: path.to_owned(),
        access: Some(Access::Permission(Permission::RolesManage)),
    }
}

fn apikeys_manage(method: Method, path: &str) -> RouteEntry {
    RouteEntry {
        method,
        path: path.to_owned(),
        access: Some(Access::Permission(Permission::ApikeysManage)),
    }
}

fn users_manage(method: Method, path: &str) -> RouteEntry {
    RouteEntry {
        method,
        path: path.to_owned(),
        access: Some(Access::Permission(Permission::UsersManage)),
    }
}

fn profiles(method: Method, path: &str, permission: Permission) -> RouteEntry {
    RouteEntry {
        method,
        path: path.to_owned(),
        access: Some(Access::Permission(permission)),
    }
}

#[test]
fn app_router_passes_route_coverage() {
    let routes = api::routes();
    assert_eq!(routes.verify(), Ok(()));
    assert_eq!(
        routes.entries(),
        &[
            RouteEntry {
                method: Method::GET,
                path: "/api/v1/health".to_owned(),
                access: Some(Access::Public),
            },
            RouteEntry {
                method: Method::GET,
                path: "/api/v1/setup".to_owned(),
                access: Some(Access::Public),
            },
            RouteEntry {
                method: Method::POST,
                path: "/api/v1/setup".to_owned(),
                access: Some(Access::Public),
            },
            RouteEntry {
                method: Method::GET,
                path: "/api/v1/me".to_owned(),
                access: Some(Access::Authenticated),
            },
            RouteEntry {
                method: Method::GET,
                path: "/api/v1/events".to_owned(),
                access: Some(Access::Authenticated),
            },
            RouteEntry {
                method: Method::POST,
                path: "/api/v1/owner/transfer".to_owned(),
                access: Some(Access::Authenticated),
            },
            RouteEntry {
                method: Method::GET,
                path: "/api/v1/permissions".to_owned(),
                access: Some(Access::Authenticated),
            },
            RouteEntry {
                method: Method::GET,
                path: "/api/v1/me/api-keys".to_owned(),
                access: Some(Access::Authenticated),
            },
            RouteEntry {
                method: Method::POST,
                path: "/api/v1/me/api-keys".to_owned(),
                access: Some(Access::Authenticated),
            },
            RouteEntry {
                method: Method::DELETE,
                path: "/api/v1/me/api-keys/{id}".to_owned(),
                access: Some(Access::Authenticated),
            },
            apikeys_manage(Method::GET, "/api/v1/api-keys"),
            apikeys_manage(Method::DELETE, "/api/v1/api-keys/{id}"),
            users_manage(Method::GET, "/api/v1/assignable-roles"),
            users_manage(Method::GET, "/api/v1/assignable-libraries"),
            roles_manage(Method::GET, "/api/v1/roles"),
            roles_manage(Method::POST, "/api/v1/roles"),
            roles_manage(Method::DELETE, "/api/v1/roles/{id}"),
            roles_manage(Method::PATCH, "/api/v1/roles/{id}"),
            users_manage(Method::GET, "/api/v1/users"),
            users_manage(Method::POST, "/api/v1/users"),
            users_manage(Method::DELETE, "/api/v1/users/{id}"),
            users_manage(Method::PATCH, "/api/v1/users/{id}"),
            users_manage(Method::PUT, "/api/v1/users/{id}/permissions"),
            RouteEntry {
                method: Method::GET,
                path: "/api/v1/audit-log".to_owned(),
                access: Some(Access::Permission(Permission::SystemAudit)),
            },
            RouteEntry {
                method: Method::GET,
                path: "/api/v1/logs".to_owned(),
                access: Some(Access::Permission(Permission::SystemLogs)),
            },
            RouteEntry {
                method: Method::GET,
                path: "/api/v1/logs/files/{name}".to_owned(),
                access: Some(Access::Permission(Permission::SystemLogs)),
            },
            profiles(
                Method::GET,
                "/api/v1/quality-profiles",
                Permission::ProfilesRead
            ),
            profiles(
                Method::POST,
                "/api/v1/quality-profiles",
                Permission::ProfilesManage
            ),
            profiles(
                Method::GET,
                "/api/v1/quality-profiles/{id}",
                Permission::ProfilesRead
            ),
            profiles(
                Method::DELETE,
                "/api/v1/quality-profiles/{id}",
                Permission::ProfilesManage
            ),
            profiles(
                Method::PATCH,
                "/api/v1/quality-profiles/{id}",
                Permission::ProfilesManage
            ),
            profiles(
                Method::GET,
                "/api/v1/custom-formats",
                Permission::ProfilesRead
            ),
            profiles(
                Method::POST,
                "/api/v1/custom-formats",
                Permission::ProfilesManage
            ),
            profiles(
                Method::POST,
                "/api/v1/custom-formats/import",
                Permission::ProfilesManage
            ),
            profiles(
                Method::GET,
                "/api/v1/custom-formats/export",
                Permission::ProfilesRead
            ),
            profiles(
                Method::POST,
                "/api/v1/custom-formats/test",
                Permission::ProfilesRead
            ),
            profiles(
                Method::GET,
                "/api/v1/custom-formats/{id}",
                Permission::ProfilesRead
            ),
            profiles(
                Method::GET,
                "/api/v1/custom-formats/{id}/export",
                Permission::ProfilesRead
            ),
            profiles(
                Method::DELETE,
                "/api/v1/custom-formats/{id}",
                Permission::ProfilesManage
            ),
            profiles(
                Method::PATCH,
                "/api/v1/custom-formats/{id}",
                Permission::ProfilesManage
            ),
            profiles(Method::POST, "/api/v1/library/scan", Permission::ImportRun),
            profiles(Method::POST, "/api/v1/library/adopt", Permission::ImportRun),
            profiles(Method::GET, "/api/v1/import-sources", Permission::ImportRun),
            profiles(
                Method::POST,
                "/api/v1/import-sources",
                Permission::ImportRun
            ),
            profiles(
                Method::PATCH,
                "/api/v1/import-sources/order",
                Permission::ImportRun
            ),
            profiles(
                Method::POST,
                "/api/v1/import-sources/test",
                Permission::ImportRun
            ),
            profiles(
                Method::DELETE,
                "/api/v1/import-sources/{id}",
                Permission::ImportRun
            ),
            profiles(
                Method::POST,
                "/api/v1/import-sources/{id}/preview",
                Permission::ImportRun
            ),
            profiles(
                Method::POST,
                "/api/v1/import-sources/{id}/run",
                Permission::ImportRun
            ),
            profiles(Method::GET, "/api/v1/movies/lookup", Permission::LibraryAdd),
            profiles(Method::GET, "/api/v1/movies", Permission::LibraryRead),
            profiles(Method::POST, "/api/v1/movies", Permission::LibraryAdd),
            profiles(Method::GET, "/api/v1/movies/{id}", Permission::LibraryRead),
            profiles(
                Method::PATCH,
                "/api/v1/movies/{id}",
                Permission::LibraryEdit
            ),
            profiles(
                Method::DELETE,
                "/api/v1/movies/{id}",
                Permission::LibraryDelete
            ),
            profiles(
                Method::POST,
                "/api/v1/movies/{id}/profiles",
                Permission::LibraryAdd
            ),
            profiles(
                Method::DELETE,
                "/api/v1/movies/{id}/profiles/{pid}",
                Permission::LibraryDelete
            ),
            profiles(
                Method::GET,
                "/api/v1/movies/{id}/files",
                Permission::LibraryRead
            ),
            profiles(
                Method::DELETE,
                "/api/v1/movies/{id}/files/{fid}",
                Permission::LibraryDelete
            ),
            profiles(
                Method::GET,
                "/api/v1/movies/{id}/releases",
                Permission::SearchRun
            ),
            profiles(
                Method::POST,
                "/api/v1/releases/grab",
                Permission::ReleaseGrab
            ),
            profiles(
                Method::GET,
                "/api/v1/language-profiles",
                Permission::LibraryRead
            ),
            profiles(
                Method::POST,
                "/api/v1/language-profiles",
                Permission::SubtitlesManage
            ),
            profiles(
                Method::PUT,
                "/api/v1/language-profiles/{id}",
                Permission::SubtitlesManage
            ),
            profiles(
                Method::DELETE,
                "/api/v1/language-profiles/{id}",
                Permission::SubtitlesManage
            ),
            profiles(
                Method::GET,
                "/api/v1/movies/{id}/subtitles",
                Permission::LibraryRead
            ),
            profiles(
                Method::PUT,
                "/api/v1/movies/{id}/language-profile",
                Permission::SubtitlesManage
            ),
            profiles(
                Method::POST,
                "/api/v1/movies/{id}/subtitles/scan",
                Permission::SubtitlesManage
            ),
            profiles(
                Method::GET,
                "/api/v1/settings/subtitles",
                Permission::LibraryRead
            ),
            profiles(
                Method::PUT,
                "/api/v1/settings/subtitles",
                Permission::SubtitlesManage
            ),
            profiles(Method::GET, "/api/v1/collections", Permission::LibraryRead),
            profiles(
                Method::GET,
                "/api/v1/collections/{id}",
                Permission::LibraryRead
            ),
            profiles(
                Method::PATCH,
                "/api/v1/collections/{id}",
                Permission::LibraryEdit
            ),
            profiles(Method::GET, "/api/v1/tags", Permission::LibraryRead),
            profiles(Method::POST, "/api/v1/tags", Permission::LibraryEdit),
            profiles(Method::DELETE, "/api/v1/tags/{id}", Permission::LibraryEdit),
            profiles(Method::PATCH, "/api/v1/tags/{id}", Permission::LibraryEdit),
            profiles(
                Method::GET,
                "/api/v1/quality-definitions",
                Permission::ProfilesRead
            ),
            profiles(
                Method::PATCH,
                "/api/v1/quality-definitions/{qualityId}",
                Permission::ProfilesManage
            ),
            profiles(
                Method::GET,
                "/api/v1/settings/tmdb",
                Permission::SettingsRead
            ),
            profiles(
                Method::PUT,
                "/api/v1/settings/tmdb",
                Permission::SettingsManage
            ),
            profiles(
                Method::GET,
                "/api/v1/settings/naming",
                Permission::SettingsRead
            ),
            profiles(
                Method::POST,
                "/api/v1/settings/naming/preview",
                Permission::SettingsRead
            ),
            profiles(
                Method::PUT,
                "/api/v1/settings/naming",
                Permission::SettingsManage
            ),
            profiles(
                Method::GET,
                "/api/v1/settings/proxy",
                Permission::SettingsRead
            ),
            profiles(
                Method::PUT,
                "/api/v1/settings/proxy",
                Permission::SettingsManage
            ),
            profiles(
                Method::POST,
                "/api/v1/settings/proxy/test",
                Permission::SettingsManage
            ),
            profiles(Method::GET, "/api/v1/libraries", Permission::SettingsRead),
            profiles(
                Method::POST,
                "/api/v1/libraries",
                Permission::SettingsManage
            ),
            profiles(
                Method::DELETE,
                "/api/v1/libraries/{id}",
                Permission::SettingsManage
            ),
            profiles(
                Method::PATCH,
                "/api/v1/libraries/{id}",
                Permission::SettingsManage
            ),
            profiles(
                Method::GET,
                "/api/v1/root-folders",
                Permission::SettingsRead
            ),
            profiles(
                Method::POST,
                "/api/v1/root-folders",
                Permission::SettingsManage
            ),
            profiles(
                Method::DELETE,
                "/api/v1/root-folders/{id}",
                Permission::SettingsManage
            ),
            profiles(
                Method::PATCH,
                "/api/v1/root-folders/{id}",
                Permission::SettingsManage
            ),
            profiles(
                Method::GET,
                "/api/v1/filesystem",
                Permission::SettingsManage
            ),
            profiles(Method::GET, "/api/v1/indexers", Permission::IndexersRead),
            profiles(Method::POST, "/api/v1/indexers", Permission::IndexersManage),
            profiles(
                Method::GET,
                "/api/v1/indexers/{id}",
                Permission::IndexersRead
            ),
            profiles(
                Method::DELETE,
                "/api/v1/indexers/{id}",
                Permission::IndexersManage
            ),
            profiles(
                Method::PATCH,
                "/api/v1/indexers/{id}",
                Permission::IndexersManage
            ),
            profiles(
                Method::POST,
                "/api/v1/indexers/{id}/test",
                Permission::IndexersManage
            ),
            profiles(
                Method::GET,
                "/api/v1/download-clients",
                Permission::ClientsRead
            ),
            profiles(
                Method::POST,
                "/api/v1/download-clients",
                Permission::ClientsManage
            ),
            profiles(
                Method::DELETE,
                "/api/v1/download-clients/{id}",
                Permission::ClientsManage
            ),
            profiles(
                Method::PATCH,
                "/api/v1/download-clients/{id}",
                Permission::ClientsManage
            ),
            profiles(
                Method::POST,
                "/api/v1/download-clients/{id}/test",
                Permission::ClientsManage
            ),
            profiles(Method::GET, "/api/v1/queue", Permission::QueueRead),
            profiles(
                Method::DELETE,
                "/api/v1/queue/{id}",
                Permission::QueueManage
            ),
        ]
    );

    let radarr = api::radarr_routes();
    assert_eq!(radarr.verify(), Ok(()));

    let sonarr = api::sonarr_routes();
    assert_eq!(sonarr.verify(), Ok(()));

    let doc = serde_json::to_value(api::openapi()).unwrap();
    assert_eq!(
        doc["paths"]["/api/v1/health"]["get"][X_PERMISSION],
        "public"
    );
    let paths = doc["paths"].as_object().unwrap();
    assert!(
        !paths
            .keys()
            .any(|path| path.starts_with("/radarr") || path.starts_with("/sonarr")),
        "compat routes must not appear in Unseenarr's OpenAPI document"
    );
}

#[test]
fn openapi_shows_x_permission_on_protected_authenticated_and_public_routes() {
    let api_router = sample_api_router();
    assert_eq!(api_router.verify(), Ok(()));

    let doc = serde_json::to_value(api_router.into_openapi()).unwrap();
    assert_eq!(
        doc["paths"]["/api/v1/test/public"]["get"][X_PERMISSION],
        "public"
    );
    assert_eq!(
        doc["paths"]["/api/v1/test/me"]["get"][X_PERMISSION],
        "authenticated"
    );
    assert_eq!(
        doc["paths"]["/api/v1/test/items"]["get"][X_PERMISSION],
        "library:read"
    );
    assert_eq!(
        doc["paths"]["/api/v1/test/items"]["post"][X_PERMISSION],
        "library:add"
    );
}

#[tokio::test]
async fn health_route_is_public_on_live_router() {
    let app = TestApp::new(None).await;
    let live = router(app.state.clone());
    let reply = app
        .send(&live, app.request("GET", "/api/v1/health", None))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["database"], "ok");
}

#[tokio::test]
async fn unauthenticated_requests_return_401_problem() {
    let app = TestApp::new(None).await;
    let router = app.mount(sample_api_router());
    let (_user_id, cookie) = app.create_user_and_sign_in(&router, "u1@example.com").await;

    for path in ["/api/v1/test/me", "/api/v1/test/items"] {
        // Missing cookie.
        let reply = app.send(&router, app.request("GET", path, None)).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(reply.content_type, "application/problem+json");
        assert_eq!(reply.body["code"], "unauthorized");

        // Forged / invalid cookie.
        let reply = app
            .send(
                &router,
                app.request("GET", path, Some("better-auth.session_token=forged.bad")),
            )
            .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(reply.content_type, "application/problem+json");
        assert_eq!(reply.body["code"], "unauthorized");
    }

    // Signed-out cookie stops working on protected routes.
    let sign_out = app
        .post_json(&router, "/api/auth/sign-out", json!({}), Some(&cookie))
        .await;
    assert_eq!(sign_out.status, StatusCode::OK);

    let reply = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/items", Some(&cookie)),
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.body["code"], "unauthorized");
}

#[tokio::test]
async fn missing_permission_returns_403_naming_required_permission() {
    let app = TestApp::new(None).await;
    let router = app.mount(sample_api_router());
    let (_user_id, cookie) = app.create_user_and_sign_in(&router, "u2@example.com").await;

    let public_reply = app
        .send(&router, app.request("GET", "/api/v1/test/public", None))
        .await;
    assert_eq!(public_reply.status, StatusCode::OK);

    let me_reply = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/me", Some(&cookie)),
        )
        .await;
    assert_eq!(me_reply.status, StatusCode::OK);
    assert_eq!(me_reply.body["permissionCount"], 0);

    let forbidden = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/items", Some(&cookie)),
        )
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    assert_eq!(forbidden.content_type, "application/problem+json");
    assert_eq!(forbidden.body["code"], "forbidden");
    assert_eq!(forbidden.body["requiredPermission"], "library:read");
    assert!(
        forbidden.body["detail"]
            .as_str()
            .unwrap()
            .contains("library:read"),
        "{}",
        forbidden.body
    );
}

#[tokio::test]
async fn role_direct_grant_and_owner_are_allowed() {
    let app = TestApp::new(None).await;
    let router = app.mount(sample_api_router());

    // Role grant.
    let (role_user, role_cookie) = app
        .create_user_and_sign_in(&router, "role@example.com")
        .await;
    let reader = app
        .create_role_with_permissions("Reader", &["library:read"])
        .await;
    app.assign_role(&role_user, reader).await;
    let reply = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/items", Some(&role_cookie)),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["userId"], role_user);
    assert_eq!(reply.body["isOwner"], false);

    // Direct user grant.
    let (direct_user, direct_cookie) = app
        .create_user_and_sign_in(&router, "direct@example.com")
        .await;
    app.grant_direct(&direct_user, "library:read").await;
    let reply = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/items", Some(&direct_cookie)),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["userId"], direct_user);

    // Built-in Owner role grants every permission without explicit permission rows.
    let (owner_user, owner_cookie) = app
        .create_user_and_sign_in(&router, "owner@example.com")
        .await;
    let owner_role = roles::Entity::find()
        .filter(roles::Column::IsOwner.eq(true))
        .one(&app.state.db)
        .await
        .unwrap()
        .expect("Owner role seeded by migration");
    app.assign_role(&owner_user, owner_role.id).await;

    let read_reply = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/items", Some(&owner_cookie)),
        )
        .await;
    assert_eq!(read_reply.status, StatusCode::OK);
    assert_eq!(read_reply.body["isOwner"], true);

    let add_reply = app
        .send(
            &router,
            app.request("POST", "/api/v1/test/items", Some(&owner_cookie)),
        )
        .await;
    assert_eq!(add_reply.status, StatusCode::CREATED);
}

#[tokio::test]
async fn removing_role_returns_403_on_next_request_without_signing_in_again() {
    let app = TestApp::new(None).await;
    let router = app.mount(sample_api_router());
    let (user_id, cookie) = app
        .create_user_and_sign_in(&router, "revoked@example.com")
        .await;
    let reader = app
        .create_role_with_permissions("Reader", &["library:read"])
        .await;
    app.assign_role(&user_id, reader).await;

    let allowed = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/items", Some(&cookie)),
        )
        .await;
    assert_eq!(allowed.status, StatusCode::OK);

    // Revoke the role in SQLite while keeping the user's session intact.
    user_roles::Entity::delete_many()
        .filter(user_roles::Column::UserId.eq(&user_id))
        .exec(&app.state.db)
        .await
        .unwrap();

    let denied = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/items", Some(&cookie)),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.body["code"], "forbidden");
    assert_eq!(denied.body["requiredPermission"], "library:read");
}

#[tokio::test]
async fn same_path_different_methods_enforce_different_permissions() {
    let app = TestApp::new(None).await;
    let router = app.mount(sample_api_router());
    let (user_id, cookie) = app
        .create_user_and_sign_in(&router, "methods@example.com")
        .await;
    let reader = app
        .create_role_with_permissions("Reader", &["library:read"])
        .await;
    app.assign_role(&user_id, reader).await;

    let get_reply = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/items", Some(&cookie)),
        )
        .await;
    assert_eq!(get_reply.status, StatusCode::OK);

    let post_reply = app
        .send(
            &router,
            app.request("POST", "/api/v1/test/items", Some(&cookie)),
        )
        .await;
    assert_eq!(post_reply.status, StatusCode::FORBIDDEN);
    assert_eq!(post_reply.body["requiredPermission"], "library:add");

    let delete_reply = app
        .send(
            &router,
            app.request("DELETE", "/api/v1/test/items", Some(&cookie)),
        )
        .await;
    assert_eq!(delete_reply.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn secure_cookie_works_with_https_base_url() {
    let app = TestApp::new(Some("https://unseenarr.example.com")).await;
    let router = app.mount(sample_api_router());
    let (user_id, cookie) = app
        .create_user_and_sign_in(&router, "https@example.com")
        .await;
    assert!(
        cookie.starts_with("__Secure-better-auth.session_token="),
        "{cookie}"
    );
    app.grant_direct(&user_id, "library:read").await;

    let reply = app
        .send(
            &router,
            app.request("GET", "/api/v1/test/items", Some(&cookie)),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["userId"], user_id);
}

#[tokio::test]
async fn require_extractor_works_directly() {
    let app = TestApp::new(None).await;
    let auth = app.state.auth.clone().axum_router_with_state();
    let direct_router = Router::new()
        .route("/direct", get(test_list_items))
        .nest("/api/auth", auth)
        .with_state(app.state.clone());

    // 401 without credentials.
    let unauth = app
        .send(&direct_router, app.request("GET", "/direct", None))
        .await;
    assert_eq!(unauth.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unauth.body["code"], "unauthorized");

    // 403 without permission.
    let (user_id, cookie) = app
        .create_user_and_sign_in(&direct_router, "extractor@example.com")
        .await;
    let forbidden = app
        .send(&direct_router, app.request("GET", "/direct", Some(&cookie)))
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    assert_eq!(forbidden.body["requiredPermission"], "library:read");

    // 200 with permission.
    app.grant_direct(&user_id, "library:read").await;
    let allowed = app
        .send(&direct_router, app.request("GET", "/direct", Some(&cookie)))
        .await;
    assert_eq!(allowed.status, StatusCode::OK);
    assert_eq!(allowed.body["userId"], user_id);
}
