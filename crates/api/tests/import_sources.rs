use api::{AppState, router};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use better_auth::seaorm::sea_orm::{ActiveModelTrait, Set};
use db::entities::{role_permissions, roles, user_roles};
use db::import_sources as store;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HOST: &str = "unseenarr.test";
const ORIGIN: &str = "http://unseenarr.test";
const PASSWORD: &str = "correct horse";

struct App {
    router: Router,
    state: AppState,
    owner: String,
    _dir: TempDir,
}

impl App {
    /// `tmdb` is the mock server that stands in for TMDB.
    async fn new(tmdb: &MockServer) -> Self {
        let (db, dir) = db::testing::temp_db().await;
        let mut state = AppState::new(db, "test secret", None).await.unwrap();
        state.tmdb_base_url = tmdb.uri();
        let mut app = Self {
            router: router(state.clone()),
            state,
            owner: String::new(),
            _dir: dir,
        };
        let body = json!({ "name": "Owner", "email": "owner@example.com", "password": PASSWORD });
        let (status, cookie, reply) = app
            .send(Method::POST, "/api/v1/setup", None, Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{reply}");
        app.owner = cookie.unwrap();
        app
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Option<String>, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, HOST)
            .header(header::ORIGIN, ORIGIN);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap())
            .find(|cookie| cookie.contains("session_token="))
            .map(|cookie| cookie.split(';').next().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            cookie,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn owner(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (status, _, body) = self.send(method, path, Some(&self.owner), body).await;
        (status, body)
    }

    /// A user who holds a new role with `permissions`. Returns the user's
    /// cookie and the role id.
    async fn member(&self, email: &str, permissions: &[&str]) -> (String, String, i32) {
        self.state.policy.admit_next_signup();
        let body = json!({ "name": "Member", "email": email, "password": PASSWORD });
        let (status, cookie, reply) = self
            .send(Method::POST, "/api/auth/sign-up/email", None, Some(body))
            .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        let user_id = reply["user"]["id"].as_str().unwrap().to_owned();
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
            all_libraries: Set(true),
        }
        .insert(&self.state.db)
        .await
        .unwrap();
        (user_id, cookie.unwrap(), role.id)
    }
}

const SOURCES: &str = "/api/v1/import-sources";
const KEY: &str = "fixture-key-0123456789abcdef";
const COLLECTION: &str = include_str!("../../../fixtures/tmdb/collection_2344.json");

const ROOT_FOLDERS: &str = include_str!("../../../fixtures/radarr/rootfolder.json");
const TAGS: &str = include_str!("../../../fixtures/radarr/tag.json");
const CUSTOM_FORMATS: &str = include_str!("../../../fixtures/radarr/customformat.json");
const PROFILES: &str = include_str!("../../../fixtures/radarr/qualityprofile.json");
const COLLECTIONS: &str = include_str!("../../../fixtures/radarr/collection.json");
const MOVIES: &str = include_str!("../../../fixtures/radarr/movie.json");
const INDEXERS: &str = include_str!("../../../fixtures/radarr/indexer.json");
const DOWNLOAD_CLIENTS: &str = include_str!("../../../fixtures/radarr/downloadclient.json");
const DEFINITIONS: &str = include_str!("../../../fixtures/radarr/qualitydefinition.json");

const RESOURCES: [&str; 10] = [
    store::ROOT_FOLDER,
    store::QUALITY_PROFILE,
    store::CUSTOM_FORMAT,
    store::TAG,
    store::COLLECTION,
    store::MOVIE,
    store::MOVIE_PROFILE,
    store::MOVIE_FILE,
    store::INDEXER,
    store::DOWNLOAD_CLIENT,
];

fn json_body(body: &str, status: u16) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

/// The bodies of a fake Radarr, one per endpoint the import reads.
struct Fake(Vec<(&'static str, String)>);

impl Fake {
    /// Every endpoint serves its fixture.
    fn new() -> Self {
        Self(vec![
            ("rootfolder", ROOT_FOLDERS.to_owned()),
            ("tag", TAGS.to_owned()),
            ("customformat", CUSTOM_FORMATS.to_owned()),
            ("qualityprofile", PROFILES.to_owned()),
            ("collection", COLLECTIONS.to_owned()),
            ("movie", MOVIES.to_owned()),
            ("indexer", INDEXERS.to_owned()),
            ("downloadclient", DOWNLOAD_CLIENTS.to_owned()),
            ("qualitydefinition", DEFINITIONS.to_owned()),
        ])
    }

    fn with(mut self, endpoint: &str, body: Value) -> Self {
        let slot = self
            .0
            .iter_mut()
            .find(|(name, _)| *name == endpoint)
            .unwrap();
        slot.1 = body.to_string();
        self
    }

    /// Serves the endpoints and expects each to be read `runs` times. Any
    /// request that is not a GET fails the test: the import never writes to
    /// Radarr.
    async fn mount(self, server: &MockServer, runs: u64) {
        for (endpoint, body) in self.0 {
            Mock::given(method("GET"))
                .and(path(format!("/api/v3/{endpoint}")))
                .respond_with(json_body(&body, 200))
                .expect(runs)
                .mount(server)
                .await;
        }
        for verb in ["POST", "PUT", "PATCH", "DELETE"] {
            Mock::given(method(verb))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(server)
                .await;
        }
    }
}

/// An app whose TMDB serves the Matrix collection.
async fn app() -> (App, MockServer) {
    let tmdb = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/collection/2344"))
        .respond_with(json_body(COLLECTION, 200))
        .mount(&tmdb)
        .await;
    let app = App::new(&tmdb).await;
    db::settings::set_secret(
        &app.state.db,
        &app.state.secrets,
        "tmdb.api_key",
        &db::secret::Secret::new(KEY),
    )
    .await
    .unwrap();
    (app, tmdb)
}

async fn add_source(app: &App, name: &str, radarr: &MockServer, target: Option<i64>) -> i32 {
    let mut body = json!({
        "kind": "radarr", "name": name, "url": radarr.uri(), "apiKey": KEY,
    });
    if let Some(target) = target {
        body["targetQualityProfileId"] = json!(target);
    }
    let (status, source) = app.owner(Method::POST, SOURCES, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{source}");
    i32::try_from(source["id"].as_i64().unwrap()).unwrap()
}

async fn import(app: &App, id: i32, action: &str) -> Value {
    let (status, report) = app
        .owner(Method::POST, &format!("{SOURCES}/{id}/{action}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    report
}

async fn last_import_at(app: &App, id: i32) -> Option<String> {
    store::list(&app.state.db)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == id)
        .unwrap()
        .last_import_at
}

/// How many rows of each kind the library has.
async fn rows(app: &App) -> [usize; 5] {
    let db = &app.state.db;
    [
        db::movies::list(db).await.unwrap().len(),
        db::profiles::list(db).await.unwrap().len(),
        db::custom_formats::list(db).await.unwrap().len(),
        db::tags::list(db).await.unwrap().len(),
        db::root_folders::list(db).await.unwrap().len(),
    ]
}

fn created(report: &Value, kind: &str) -> u64 {
    report[kind]["created"].as_u64().unwrap()
}

/// Every kind of resource in a report with its `created` count.
fn all_created(report: &Value) -> Vec<(String, u64)> {
    report
        .as_object()
        .unwrap()
        .iter()
        .filter(|(name, _)| *name != "notes" && *name != "mergedMovies")
        .map(|(name, counts)| (name.clone(), counts["created"].as_u64().unwrap()))
        .collect()
}

/// The Matrix as a source reports it, with one file.
fn matrix(profile: &Value, root: &str, file: &str, size: i64) -> Value {
    let mut movie: Value = serde_json::from_str::<Vec<Value>>(MOVIES)
        .unwrap()
        .remove(0);
    movie["collection"] = Value::Null;
    movie["tags"] = json!([]);
    movie["qualityProfileId"] = profile["id"].clone();
    movie["rootFolderPath"] = json!(root);
    movie["path"] = json!(format!("{root}/The Matrix (1999)"));
    movie["folderName"] = json!("The Matrix (1999)");
    movie["movieFile"]["path"] = json!(format!("{root}/The Matrix (1999)/{file}"));
    movie["movieFile"]["relativePath"] = json!(file);
    movie["movieFile"]["size"] = json!(size);
    movie
}

fn fixture_profile(index: usize) -> Value {
    serde_json::from_str::<Vec<Value>>(PROFILES)
        .unwrap()
        .remove(index)
}

/// A source whose only movie is the Matrix in `root`, with `profile`.
fn single(profile: &Value, root: &str, file: &str, size: i64) -> Fake {
    Fake::new()
        .with(
            "rootfolder",
            json!([{ "path": root, "accessible": true, "freeSpace": 1,
                     "unmappedFolders": [], "id": 1 }]),
        )
        .with("qualityprofile", json!([profile]))
        .with("collection", json!([]))
        .with("indexer", json!([]))
        .with("downloadclient", json!([]))
        .with("movie", json!([matrix(profile, root, file, size)]))
}

#[tokio::test]
async fn a_run_imports_an_instance_and_a_second_run_creates_nothing() {
    let (app, _tmdb) = app().await;
    let radarr = MockServer::start().await;
    let mut clients: Vec<Value> = serde_json::from_str(DOWNLOAD_CLIENTS).unwrap();
    clients[0]["tags"] = json!([2]);
    Fake::new()
        .with("downloadclient", json!(clients))
        .mount(&radarr, 2)
        .await;
    let id = add_source(&app, "Main", &radarr, None).await;

    let first = import(&app, id, "run").await;
    let local_tag = |label: &str| {
        let label = label.to_owned();
        let db = app.state.db.clone();
        async move {
            db::tags::list(&db)
                .await
                .unwrap()
                .into_iter()
                .find(|tag| tag.label == label)
                .unwrap()
                .id
        }
    };
    let indexers = db::indexers::list(&app.state.db).await.unwrap();
    assert_eq!(indexers.len(), 1, "{first}");
    assert_eq!(indexers[0].tags, vec![local_tag("4k").await]);
    let clients = db::download_clients::list(&app.state.db).await.unwrap();
    assert_eq!(clients.len(), 1, "{first}");
    assert_eq!(clients[0].tags, vec![local_tag("family").await]);
    let counts = all_created(&first);
    for kind in ["rootFolders", "qualityProfiles", "tags", "movies"] {
        assert!(created(&first, kind) > 0, "{kind}: {first}");
    }
    assert!(!db::movies::list(&app.state.db).await.unwrap().is_empty());
    assert!(last_import_at(&app, id).await.is_some());
    let after_first = rows(&app).await;

    let second = import(&app, id, "run").await;
    for (kind, count) in all_created(&second) {
        assert_eq!(count, 0, "{kind} created again: {second}");
    }
    assert_eq!(rows(&app).await, after_first);
    assert!(counts.iter().any(|(_, count)| *count > 0));
}

#[tokio::test]
async fn a_preview_writes_nothing() {
    let (app, _tmdb) = app().await;
    let radarr = MockServer::start().await;
    Fake::new().mount(&radarr, 2).await;
    let id = add_source(&app, "Main", &radarr, None).await;
    let before = rows(&app).await;

    let preview = import(&app, id, "preview").await;
    assert!(created(&preview, "movies") > 0, "{preview}");
    assert_eq!(rows(&app).await, before);
    for resource in RESOURCES {
        let mapped = store::map_for(&app.state.db, id, resource).await.unwrap();
        assert!(mapped.is_empty(), "{resource} was mapped");
    }
    assert_eq!(last_import_at(&app, id).await, None);

    let run = import(&app, id, "run").await;
    assert_eq!(created(&run, "movies"), created(&preview, "movies"));
    assert!(rows(&app).await[0] > 0);
    assert!(last_import_at(&app, id).await.is_some());
}

#[tokio::test]
async fn the_target_quality_profile_of_a_source_is_used() {
    let (app, _tmdb) = app().await;
    let (status, profile) = app
        .owner(
            Method::POST,
            "/api/v1/quality-profiles",
            Some(json!({
                "name": "Target",
                "cutoff": 7,
                "items": [
                    { "type": "quality", "qualityId": 4, "allowed": true },
                    { "type": "quality", "qualityId": 7, "allowed": true },
                ],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{profile}");
    let target = profile["id"].as_i64().unwrap();
    let radarr = MockServer::start().await;
    Fake::new().mount(&radarr, 1).await;
    let id = add_source(&app, "Main", &radarr, Some(target)).await;

    let report = import(&app, id, "run").await;
    assert_eq!(created(&report, "qualityProfiles"), 0, "{report}");
    assert!(created(&report, "movies") > 0, "{report}");
    let movies = db::movies::list(&app.state.db).await.unwrap();
    assert!(!movies.is_empty());
    for movie in movies {
        let used: Vec<i64> = movie
            .profiles
            .iter()
            .map(|link| i64::from(link.quality_profile_id))
            .collect();
        assert_eq!(used, vec![target], "{}", movie.model.title);
    }
}

#[tokio::test]
async fn two_sources_merge_into_one_movie_with_two_profiles_and_two_files() {
    let (app, _tmdb) = app().await;
    let (hd, uhd) = (fixture_profile(0), fixture_profile(1));
    let (a, b) = (MockServer::start().await, MockServer::start().await);
    single(&hd, "/movies", "hd.mkv", 1_000).mount(&a, 1).await;
    single(&uhd, "/movies-4k", "uhd.mkv", 5_000)
        .mount(&b, 2)
        .await;
    let source_a = add_source(&app, "HD", &a, None).await;
    let source_b = add_source(&app, "UHD", &b, None).await;

    let first = import(&app, source_a, "run").await;
    assert_eq!(created(&first, "movies"), 1, "{first}");
    let report = import(&app, source_b, "run").await;
    assert_eq!(created(&report, "movies"), 0, "{report}");
    assert_eq!(report["movies"]["matched"], 1, "{report}");
    let title = matrix(&uhd, "/movies-4k", "uhd.mkv", 5_000)["title"].clone();
    assert_eq!(report["mergedMovies"], json!([title]), "{report}");

    let movies = db::movies::list(&app.state.db).await.unwrap();
    let matrix: Vec<_> = movies.iter().filter(|m| m.model.tmdb_id == 603).collect();
    assert_eq!(matrix.len(), 1);
    let movie = matrix[0];
    assert_eq!(movie.profiles.len(), 2);

    let profile_a = store::map_for(&app.state.db, source_a, store::QUALITY_PROFILE)
        .await
        .unwrap()[&i32::try_from(hd["id"].as_i64().unwrap()).unwrap()];
    let primary = movie.profiles.iter().find(|link| link.is_primary).unwrap();
    assert_eq!(primary.quality_profile_id, profile_a);

    let files = db::movies::files_of(&app.state.db, movie.model.id)
        .await
        .unwrap();
    assert_eq!(files.len(), 2);
    let mut linked: Vec<i32> = movie.profiles.iter().filter_map(|l| l.file_id).collect();
    linked.sort_unstable();
    let mut ids: Vec<i32> = files.iter().map(|f| f.id).collect();
    ids.sort_unstable();
    assert_eq!(linked, ids, "each profile has its own file");
    let first_file = files.iter().find(|f| f.relative_path == "hd.mkv").unwrap();
    assert_eq!(first_file.size, 1_000);
    let second_file = files.iter().find(|f| f.size == 5_000).unwrap();
    assert_eq!(
        second_file.relative_path,
        "/movies-4k/The Matrix (1999)/uhd.mkv"
    );

    let again = import(&app, source_b, "run").await;
    for (kind, count) in all_created(&again) {
        assert_eq!(count, 0, "{kind} created again: {again}");
    }
    let files = db::movies::files_of(&app.state.db, movie.model.id)
        .await
        .unwrap();
    assert_eq!(files.len(), 2);
}

#[tokio::test]
async fn two_sources_with_the_same_profile_name_but_different_scores_are_kept_apart() {
    let (app, _tmdb) = app().await;
    let first = fixture_profile(0);
    let mut second = first.clone();
    second["formatItems"][1]["score"] = json!(-500);
    let (a, b) = (MockServer::start().await, MockServer::start().await);
    single(&first, "/movies", "a.mkv", 1_000).mount(&a, 1).await;
    single(&second, "/movies", "b.mkv", 2_000)
        .mount(&b, 1)
        .await;
    let source_a = add_source(&app, "A", &a, None).await;
    let source_b = add_source(&app, "B", &b, None).await;

    import(&app, source_a, "run").await;
    let report = import(&app, source_b, "run").await;
    assert_eq!(created(&report, "qualityProfiles"), 1, "{report}");

    let name = first["name"].as_str().unwrap();
    let mut names: Vec<String> = db::profiles::list(&app.state.db)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.name)
        .filter(|row| row.starts_with(name))
        .collect();
    names.sort();
    // A default profile already has this name, so each source's copy is
    // suffixed with the source name.
    let mut expected = vec![
        name.to_owned(),
        format!("{name} (A)"),
        format!("{name} (B)"),
    ];
    expected.sort();
    assert_eq!(names, expected);
    assert_eq!(
        db::custom_formats::list(&app.state.db).await.unwrap().len(),
        2
    );
    let notes = report["notes"].to_string();
    assert!(notes.contains(&format!("{name} (B)")), "{notes}");
}

#[tokio::test]
async fn a_member_without_import_run_cannot_use_the_source() {
    let (app, _tmdb) = app().await;
    let radarr = MockServer::start().await;
    Fake::new().mount(&radarr, 0).await;
    let id = add_source(&app, "Main", &radarr, None).await;
    let (_, cookie, _) = app.member("viewer@example.com", &["library:read"]).await;

    let (status, _, body) = app
        .send(
            Method::POST,
            &format!("{SOURCES}/{id}/preview"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// The sources in the order the list returns them.
async fn source_names(app: &App) -> Vec<String> {
    let (status, body) = app.owner(Method::GET, SOURCES, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array()
        .unwrap()
        .iter()
        .map(|source| source["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn the_order_of_sources_can_be_changed() {
    let (app, _tmdb) = app().await;
    let (a, b) = (MockServer::start().await, MockServer::start().await);
    let source_a = add_source(&app, "A", &a, None).await;
    let source_b = add_source(&app, "B", &b, None).await;
    assert_eq!(source_names(&app).await, ["A", "B"]);

    let (status, body) = app
        .owner(
            Method::PATCH,
            &format!("{SOURCES}/order"),
            Some(json!({ "ids": [source_b, source_a] })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(source_names(&app).await, ["B", "A"]);

    let (status, body) = app
        .owner(
            Method::PATCH,
            &format!("{SOURCES}/order"),
            Some(json!({ "ids": [source_b] })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(source_names(&app).await, ["B", "A"]);
}

#[tokio::test]
async fn a_source_imports_only_the_groups_it_lists() {
    let (app, _tmdb) = app().await;
    let radarr = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/rootfolder"))
        .respond_with(json_body(ROOT_FOLDERS, 200))
        .expect(1)
        .mount(&radarr)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/qualitydefinition"))
        .respond_with(json_body(DEFINITIONS, 200))
        .expect(1)
        .mount(&radarr)
        .await;
    let (status, source) = app
        .owner(
            Method::POST,
            SOURCES,
            Some(json!({
                "kind": "radarr", "name": "Roots", "url": radarr.uri(), "apiKey": KEY,
                "resources": ["rootFolders"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{source}");
    assert_eq!(source["resources"], json!(["rootFolders"]));
    assert_eq!(source["importQualityDefinitions"], json!(false));
    let id = i32::try_from(source["id"].as_i64().unwrap()).unwrap();

    let report = import(&app, id, "run").await;
    assert!(created(&report, "rootFolders") > 0, "{report}");
    assert_eq!(created(&report, "movies"), 0, "{report}");
    assert_eq!(report["movies"]["skipped"], json!(0), "{report}");
    assert_eq!(db::movies::list(&app.state.db).await.unwrap().len(), 0);
    assert_eq!(created(&report, "qualityDefinitions"), 0, "{report}");
}

/// Adds a source with extra body fields.
async fn add_source_extra(app: &App, name: &str, radarr: &MockServer, extra: Value) -> i32 {
    let mut body = json!({
        "kind": "radarr", "name": name, "url": radarr.uri(), "apiKey": KEY,
    });
    if let Value::Object(extra) = extra {
        for (key, value) in extra {
            body[key] = value;
        }
    }
    let (status, source) = app.owner(Method::POST, SOURCES, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{source}");
    i32::try_from(source["id"].as_i64().unwrap()).unwrap()
}

#[tokio::test]
async fn quality_definitions_are_applied_only_when_asked() {
    let (app, _tmdb) = app().await;
    let mut definitions: Vec<Value> = serde_json::from_str(DEFINITIONS).unwrap();
    let quality_id = i32::try_from(definitions[0]["quality"]["id"].as_i64().unwrap()).unwrap();
    let before = db::quality_definitions::find(&app.state.db, quality_id)
        .await
        .unwrap()
        .unwrap();
    definitions[0]["maxSize"] = json!(123.5);
    definitions[0]["minSize"] = json!(1.5);
    let changed = json!(definitions);
    let plain_server = MockServer::start().await;
    Fake::new()
        .with("qualitydefinition", changed.clone())
        .mount(&plain_server, 1)
        .await;
    let applied_server = MockServer::start().await;
    Fake::new()
        .with("qualitydefinition", changed)
        .mount(&applied_server, 1)
        .await;

    let plain = add_source(&app, "Plain", &plain_server, None).await;
    let report = import(&app, plain, "run").await;
    assert_eq!(created(&report, "qualityDefinitions"), 0, "{report}");
    assert_eq!(
        report["qualityDefinitions"]["skipped"],
        json!(definitions.len()),
        "{report}"
    );
    let row = db::quality_definitions::find(&app.state.db, quality_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row, before);

    let applied = add_source_extra(
        &app,
        "Defs",
        &applied_server,
        json!({ "importQualityDefinitions": true }),
    )
    .await;
    let report = import(&app, applied, "run").await;
    assert!(created(&report, "qualityDefinitions") > 0, "{report}");
    let row = db::quality_definitions::find(&app.state.db, quality_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.min_size, 1.5);
    assert_eq!(row.max_size, Some(123.5));
    assert_ne!(row, before);
}

const STATUS: &str = r#"{"appName":"Radarr","instanceName":"Docker","version":"5.2.6.8376"}"#;

#[tokio::test]
async fn a_connection_is_tested_without_being_saved() {
    let (app, _tmdb) = app().await;
    let radarr = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/system/status"))
        .respond_with(json_body(STATUS, 200))
        .expect(1)
        .mount(&radarr)
        .await;

    let (status, body) = app
        .owner(
            Method::POST,
            &format!("{SOURCES}/test"),
            Some(json!({ "url": radarr.uri(), "apiKey": KEY })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["appName"], json!("Radarr"));
    assert_eq!(body["instanceName"], json!("Docker"));
    assert_eq!(body["version"], json!("5.2.6.8376"));
    assert!(store::list(&app.state.db).await.unwrap().is_empty());

    let rejecting = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/system/status"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&rejecting)
        .await;
    let (status, body) = app
        .owner(
            Method::POST,
            &format!("{SOURCES}/test"),
            Some(json!({ "url": rejecting.uri(), "apiKey": KEY })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");

    let (status, body) = app
        .owner(
            Method::POST,
            &format!("{SOURCES}/test"),
            Some(json!({ "url": " ", "apiKey": KEY })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}
