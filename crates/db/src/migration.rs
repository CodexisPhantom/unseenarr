//! Versioned migrations. Never edit a released migration; add a new one.

use sea_orm::{DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(M0001Init),
            Box::new(M0002AccessControl),
            Box::new(M0003ApiKeys),
            Box::new(M0004Movies),
            Box::new(M0005AuditLog),
            Box::new(M0006Indexers),
            Box::new(M0007DownloadClients),
            Box::new(M0008DownloadClientApiKey),
            Box::new(M0009QualityDefinitions),
            Box::new(M0010Subtitles),
            Box::new(M0011Collections),
            Box::new(M0012CustomFormats),
            Box::new(M0013AuthTwoFactorPasskeys),
            Box::new(M0014DownloadsAndBlocklist),
            Box::new(M0015Libraries),
            Box::new(M0016TvShows),
            Box::new(M0017History),
            Box::new(M0018ImportSources),
            Box::new(M0019ImportSourceOrderAndOptions),
        ]
    }
}

struct M0001Init;

impl MigrationName for M0001Init {
    fn name(&self) -> &str {
        "m0001_init"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for M0001Init {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        crate::auth_schema::create_auth_tables(manager.get_connection()).await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("settings"))
                    .col(
                        ColumnDef::new(Alias::new("key"))
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("value")).json().not_null())
                    .col(ColumnDef::new(Alias::new("updated_at")).text().not_null())
                    .to_owned(),
            )
            .await
    }
}

struct M0002AccessControl;

impl MigrationName for M0002AccessControl {
    fn name(&self) -> &str {
        "m0002_access_control"
    }
}

/// Roles, grants and the `disabled` flag (docs/04-database.md, "Access
/// control tables"). Plain SQL: the partial unique index has no builder.
///
/// `user.disabled` is our column on a table of the auth library. It has a
/// default, so the library's own inserts, which do not name it, still work.
const M0002_UP: &str = r#"
ALTER TABLE "user" ADD COLUMN disabled boolean NOT NULL DEFAULT FALSE;

CREATE TABLE roles (
    id integer PRIMARY KEY AUTOINCREMENT,
    name text NOT NULL UNIQUE,
    description text,
    is_owner boolean NOT NULL DEFAULT FALSE,
    created_at text NOT NULL,
    updated_at text NOT NULL
);
-- At most one row can be the Owner role.
CREATE UNIQUE INDEX roles_single_owner ON roles (is_owner) WHERE is_owner = 1;

CREATE TABLE role_permissions (
    role_id integer NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    permission text NOT NULL,
    PRIMARY KEY (role_id, permission)
);

CREATE TABLE user_roles (
    user_id text NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    role_id integer NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role_id)
);
CREATE INDEX user_roles_role_id_idx ON user_roles (role_id);

CREATE TABLE user_permissions (
    user_id text NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    permission text NOT NULL,
    PRIMARY KEY (user_id, permission)
);

INSERT INTO roles (name, description, is_owner, created_at, updated_at)
VALUES ('Owner', 'Every permission. Built in.', TRUE,
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0002AccessControl {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0002_UP)
            .await?;
        Ok(())
    }
}

struct M0003ApiKeys;

impl MigrationName for M0003ApiKeys {
    fn name(&self) -> &str {
        "m0003_api_keys"
    }
}

/// API keys (docs/04-database.md). Only the SHA-256 of a key is stored. A
/// revoked key keeps its row, so the audit log can still name it.
const M0003_UP: &str = r#"
CREATE TABLE api_keys (
    id integer PRIMARY KEY AUTOINCREMENT,
    user_id text NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    name text NOT NULL,
    key_hash text NOT NULL UNIQUE,
    key_prefix text NOT NULL,
    last_used_at text,
    created_at text NOT NULL,
    revoked_at text
);
CREATE INDEX api_keys_user_id_idx ON api_keys (user_id);

CREATE TABLE api_key_permissions (
    api_key_id integer NOT NULL REFERENCES api_keys (id) ON DELETE CASCADE,
    permission text NOT NULL,
    PRIMARY KEY (api_key_id, permission)
);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0003ApiKeys {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0003_UP)
            .await?;
        Ok(())
    }
}

struct M0004Movies;

impl MigrationName for M0004Movies {
    fn name(&self) -> &str {
        "m0004_movies"
    }
}

/// Library tables (docs/04-database.md, "Library tables"), without
/// `collections`: the issue that adds that table also adds
/// `movies.collection_id`.
///
/// A movie has one `movie_profiles` row per attached quality profile and at
/// most one primary (ADR 0010). A file with `movie_profile_id` set is the
/// managed file of that profile; a file without is unmanaged.
const M0004_UP: &str = r#"
CREATE TABLE root_folders (
    id integer PRIMARY KEY AUTOINCREMENT,
    path text NOT NULL UNIQUE,
    created_at text NOT NULL
);

CREATE TABLE quality_profiles (
    id integer PRIMARY KEY AUTOINCREMENT,
    name text NOT NULL UNIQUE,
    upgrade_allowed boolean NOT NULL,
    cutoff_quality_id integer NOT NULL,
    items json NOT NULL,
    language text NOT NULL DEFAULT 'any',
    min_format_score integer NOT NULL DEFAULT 0,
    cutoff_format_score integer NOT NULL DEFAULT 0,
    min_upgrade_format_score integer NOT NULL DEFAULT 1,
    trash_id text,
    created_at text NOT NULL,
    updated_at text NOT NULL
);

CREATE TABLE tags (
    id integer PRIMARY KEY AUTOINCREMENT,
    label text NOT NULL UNIQUE
);

CREATE TABLE movies (
    id integer PRIMARY KEY AUTOINCREMENT,
    tmdb_id integer NOT NULL UNIQUE,
    imdb_id text,
    title text NOT NULL,
    original_title text,
    sort_title text NOT NULL,
    year integer NOT NULL,
    overview text,
    runtime integer,
    status text NOT NULL,
    in_cinemas text,
    digital_release text,
    physical_release text,
    poster_path text,
    root_folder_id integer NOT NULL REFERENCES root_folders (id),
    folder_name text NOT NULL,
    monitored boolean NOT NULL,
    minimum_availability text NOT NULL,
    added_at text NOT NULL,
    updated_at text NOT NULL
);
CREATE INDEX movies_root_folder_id_idx ON movies (root_folder_id);

CREATE TABLE movie_profiles (
    id integer PRIMARY KEY AUTOINCREMENT,
    movie_id integer NOT NULL REFERENCES movies (id) ON DELETE CASCADE,
    quality_profile_id integer NOT NULL REFERENCES quality_profiles (id),
    is_primary boolean NOT NULL,
    created_at text NOT NULL,
    UNIQUE (movie_id, quality_profile_id)
);
-- At most one primary profile per movie.
CREATE UNIQUE INDEX movie_profiles_single_primary ON movie_profiles (movie_id) WHERE is_primary = 1;
CREATE INDEX movie_profiles_quality_profile_id_idx ON movie_profiles (quality_profile_id);

CREATE TABLE movie_tags (
    movie_id integer NOT NULL REFERENCES movies (id) ON DELETE CASCADE,
    tag_id integer NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    PRIMARY KEY (movie_id, tag_id)
);
CREATE INDEX movie_tags_tag_id_idx ON movie_tags (tag_id);

CREATE TABLE movie_files (
    id integer PRIMARY KEY AUTOINCREMENT,
    movie_id integer NOT NULL REFERENCES movies (id) ON DELETE CASCADE,
    relative_path text NOT NULL,
    size integer NOT NULL,
    quality_id integer NOT NULL,
    release_group text,
    edition text,
    scene_name text,
    media_info json,
    languages json,
    movie_profile_id integer UNIQUE REFERENCES movie_profiles (id) ON DELETE SET NULL,
    added_at text NOT NULL,
    UNIQUE (movie_id, relative_path)
);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0004Movies {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0004_UP)
            .await?;
        Ok(())
    }
}

struct M0005AuditLog;

impl MigrationName for M0005AuditLog {
    fn name(&self) -> &str {
        "m0005_audit_log"
    }
}

/// The audit log (docs/04-database.md). Append-only: nothing in the app
/// updates or deletes a row. The actor columns are set to null when the user
/// or key is deleted; `actor_name` was copied at write time, so the row still
/// says who acted.
const M0005_UP: &str = r#"
CREATE TABLE audit_log (
    id integer PRIMARY KEY AUTOINCREMENT,
    created_at text NOT NULL,
    actor_user_id text REFERENCES "user" (id) ON DELETE SET NULL,
    actor_name text NOT NULL,
    api_key_id integer REFERENCES api_keys (id) ON DELETE SET NULL,
    action text NOT NULL,
    target_kind text,
    target_id text,
    data json,
    ip_address text
);
CREATE INDEX audit_log_created_at_idx ON audit_log (created_at);
CREATE INDEX audit_log_actor_user_id_idx ON audit_log (actor_user_id);
CREATE INDEX audit_log_target_idx ON audit_log (target_kind, target_id);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0005AuditLog {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0005_UP)
            .await?;
        Ok(())
    }
}

struct M0006Indexers;

impl MigrationName for M0006Indexers {
    fn name(&self) -> &str {
        "m0006_indexers"
    }
}

/// Indexers and their tags (docs/04-database.md, "Integration tables").
/// `api_key` holds the encrypted form (ADR 0011).
const M0006_UP: &str = r#"
CREATE TABLE indexers (
    id integer PRIMARY KEY AUTOINCREMENT,
    name text NOT NULL UNIQUE,
    kind text NOT NULL,
    url text NOT NULL,
    api_key text NOT NULL,
    categories json NOT NULL,
    priority integer NOT NULL DEFAULT 25,
    use_proxy boolean NOT NULL DEFAULT TRUE,
    enabled boolean NOT NULL,
    created_at text NOT NULL
);

CREATE TABLE indexer_tags (
    indexer_id integer NOT NULL REFERENCES indexers (id) ON DELETE CASCADE,
    tag_id integer NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    PRIMARY KEY (indexer_id, tag_id)
);
CREATE INDEX indexer_tags_tag_id_idx ON indexer_tags (tag_id);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0006Indexers {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0006_UP)
            .await?;
        Ok(())
    }
}

struct M0007DownloadClients;

impl MigrationName for M0007DownloadClients {
    fn name(&self) -> &str {
        "m0007_download_clients"
    }
}

/// Download clients and their tags (docs/04-database.md, "Integration
/// tables"). `password` holds the encrypted form (ADR 0011). `remote_path`
/// and `local_path` are the client's remote path mapping: both set or both
/// null.
const M0007_UP: &str = r#"
CREATE TABLE download_clients (
    id integer PRIMARY KEY AUTOINCREMENT,
    name text NOT NULL UNIQUE,
    kind text NOT NULL,
    url text NOT NULL,
    username text,
    password text,
    category text NOT NULL,
    remote_path text,
    local_path text,
    remove_completed boolean NOT NULL DEFAULT FALSE,
    enabled boolean NOT NULL,
    created_at text NOT NULL,
    CHECK ((remote_path IS NULL) = (local_path IS NULL))
);

CREATE TABLE download_client_tags (
    download_client_id integer NOT NULL REFERENCES download_clients (id) ON DELETE CASCADE,
    tag_id integer NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    PRIMARY KEY (download_client_id, tag_id)
);
CREATE INDEX download_client_tags_tag_id_idx ON download_client_tags (tag_id);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0007DownloadClients {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0007_UP)
            .await?;
        Ok(())
    }
}

struct M0008DownloadClientApiKey;

impl MigrationName for M0008DownloadClientApiKey {
    fn name(&self) -> &str {
        "m0008_download_client_api_key"
    }
}

/// The API key of qBittorrent 5.2 or later, as an alternative to the user
/// name and password. Encrypted like the password (ADR 0011).
const M0008_UP: &str = "ALTER TABLE download_clients ADD COLUMN api_key text;";

#[async_trait::async_trait]
impl MigrationTrait for M0008DownloadClientApiKey {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0008_UP)
            .await?;
        Ok(())
    }
}

struct M0009QualityDefinitions;

impl MigrationName for M0009QualityDefinitions {
    fn name(&self) -> &str {
        "m0009_quality_definitions"
    }
}

/// Size limits per quality, in megabytes per minute of runtime, seeded with
/// Radarr's defaults (`GET /api/v3/qualitydefinition` of a new Radarr 7.0.3;
/// the same values as `Quality.DefaultQualityDefinitions` in its source). A
/// null `max_size` is no limit; a null `preferred_size` is no preference.
const M0009_UP: &str = r#"
CREATE TABLE quality_definitions (
    quality_id integer PRIMARY KEY,
    title text NOT NULL,
    min_size real NOT NULL,
    preferred_size real,
    max_size real
);

INSERT INTO quality_definitions (quality_id, title, min_size, preferred_size, max_size) VALUES
    (0, 'Unknown', 0, 95, 100),
    (24, 'WORKPRINT', 0, 95, 100),
    (25, 'CAM', 0, 95, 100),
    (26, 'TELESYNC', 0, 95, 100),
    (27, 'TELECINE', 0, 95, 100),
    (29, 'REGIONAL', 0, 95, 100),
    (28, 'DVDSCR', 0, 95, 100),
    (1, 'SDTV', 0, 95, 100),
    (2, 'DVD', 0, 95, 100),
    (23, 'DVD-R', 0, 95, 100),
    (8, 'WEBDL-480p', 0, 95, 100),
    (12, 'WEBRip-480p', 0, 95, 100),
    (20, 'Bluray-480p', 0, 95, 100),
    (21, 'Bluray-576p', 0, 95, 100),
    (4, 'HDTV-720p', 0, 95, 100),
    (5, 'WEBDL-720p', 0, 95, 100),
    (14, 'WEBRip-720p', 0, 95, 100),
    (6, 'Bluray-720p', 0, 95, 100),
    (9, 'HDTV-1080p', 0, 95, 100),
    (3, 'WEBDL-1080p', 0, 95, 100),
    (15, 'WEBRip-1080p', 0, 95, 100),
    (7, 'Bluray-1080p', 0, NULL, NULL),
    (30, 'Remux-1080p', 0, NULL, NULL),
    (16, 'HDTV-2160p', 0, NULL, NULL),
    (18, 'WEBDL-2160p', 0, NULL, NULL),
    (17, 'WEBRip-2160p', 0, NULL, NULL),
    (19, 'Bluray-2160p', 0, NULL, NULL),
    (31, 'Remux-2160p', 0, NULL, NULL),
    (22, 'BR-DISK', 0, NULL, NULL),
    (10, 'Raw-HD', 0, NULL, NULL);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0009QualityDefinitions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0009_UP)
            .await?;
        Ok(())
    }
}

struct M0010Subtitles;

impl MigrationName for M0010Subtitles {
    fn name(&self) -> &str {
        "m0010_subtitles"
    }
}

/// Subtitle tables (docs/04-database.md, "Subtitle tables (M10)"). A movie
/// has at most one language profile; a profile that a movie uses cannot be
/// deleted. A file has at most one subtitle per language and kind.
const M0010_UP: &str = r#"
CREATE TABLE language_profiles (
    id integer PRIMARY KEY AUTOINCREMENT,
    name text NOT NULL UNIQUE,
    items json NOT NULL,
    cutoff integer,
    created_at text NOT NULL,
    updated_at text NOT NULL
);

ALTER TABLE movies ADD COLUMN language_profile_id integer
    REFERENCES language_profiles (id) ON DELETE RESTRICT;
CREATE INDEX movies_language_profile_id_idx ON movies (language_profile_id);

CREATE TABLE subtitles (
    id integer PRIMARY KEY AUTOINCREMENT,
    movie_file_id integer NOT NULL REFERENCES movie_files (id) ON DELETE CASCADE,
    language text NOT NULL,
    kind text NOT NULL CHECK (kind IN ('normal', 'hi', 'forced')),
    relative_path text NOT NULL,
    provider text,
    provider_id text,
    score integer,
    added_at text NOT NULL,
    UNIQUE (movie_file_id, language, kind)
);
CREATE INDEX subtitles_movie_file_id_idx ON subtitles (movie_file_id);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0010Subtitles {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0010_UP)
            .await?;
        Ok(())
    }
}

struct M0011Collections;

impl MigrationName for M0011Collections {
    fn name(&self) -> &str {
        "m0011_collections"
    }
}

/// Collections and the link from a movie to its collection (docs/04-database.md,
/// "Library tables"). A collection outlives its movies: deleting a movie
/// leaves the row, and deleting a root folder or quality profile clears the
/// defaults of the collection.
const M0011_UP: &str = r#"
CREATE TABLE collections (
    id integer PRIMARY KEY AUTOINCREMENT,
    tmdb_id integer NOT NULL UNIQUE,
    title text NOT NULL,
    sort_title text NOT NULL,
    overview text,
    poster_path text,
    monitored boolean NOT NULL DEFAULT FALSE,
    root_folder_id integer REFERENCES root_folders (id) ON DELETE SET NULL,
    quality_profile_id integer REFERENCES quality_profiles (id) ON DELETE SET NULL,
    minimum_availability text NOT NULL,
    added_at text NOT NULL,
    updated_at text NOT NULL
);

ALTER TABLE movies ADD COLUMN collection_id integer REFERENCES collections (id) ON DELETE SET NULL;
CREATE INDEX movies_collection_id_idx ON movies (collection_id);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0011Collections {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0011_UP)
            .await?;
        Ok(())
    }
}

/// Custom formats and their scores per quality profile
/// (docs/04-database.md). A format has at most one Trash Guide id; two rows
/// with no id are allowed. Deleting a format or a profile deletes its link
/// rows.
const M0012_UP: &str = r#"
CREATE TABLE custom_formats (
    id integer PRIMARY KEY AUTOINCREMENT,
    name text NOT NULL UNIQUE,
    include_when_renaming boolean NOT NULL DEFAULT FALSE,
    specifications json NOT NULL,
    trash_id text UNIQUE,
    trash_synced boolean NOT NULL DEFAULT FALSE,
    created_at text NOT NULL,
    updated_at text NOT NULL
);
CREATE TABLE quality_profile_custom_formats (
    quality_profile_id integer NOT NULL REFERENCES quality_profiles (id) ON DELETE CASCADE,
    custom_format_id integer NOT NULL REFERENCES custom_formats (id) ON DELETE CASCADE,
    score integer NOT NULL,
    PRIMARY KEY (quality_profile_id, custom_format_id)
);
"#;

struct M0012CustomFormats;

impl MigrationName for M0012CustomFormats {
    fn name(&self) -> &str {
        "m0012_custom_formats"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for M0012CustomFormats {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0012_UP)
            .await?;
        Ok(())
    }
}

struct M0013AuthTwoFactorPasskeys;

impl MigrationName for M0013AuthTwoFactorPasskeys {
    fn name(&self) -> &str {
        "m0013_auth_two_factor_passkeys"
    }
}

/// Two-factor and passkey storage (#112). Both tables are plugin-owned and
/// come from the regenerated `auth_schema.rs`; this migration adds them to a
/// database that `m0001_init` created before the plugins were enabled.
///
/// The guards let one migration serve both cases: a fresh database already
/// gets the column and tables from `create_auth_tables`, while an existing one
/// gets them here. Nothing collides when both run in the same startup.
const M0013_UP: &str = r#"
CREATE TABLE IF NOT EXISTS "twoFactor" (
    id text NOT NULL PRIMARY KEY,
    secret text NOT NULL,
    "backupCodes" text NOT NULL,
    "userId" text NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    verified integer,
    "failedVerificationCount" integer,
    "lockedUntil" date
);
CREATE INDEX IF NOT EXISTS twoFactor_secret_idx ON "twoFactor" (secret);
CREATE INDEX IF NOT EXISTS twoFactor_userId_idx ON "twoFactor" ("userId");

CREATE TABLE IF NOT EXISTS passkey (
    id text NOT NULL PRIMARY KEY,
    name text,
    "publicKey" text NOT NULL,
    "userId" text NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    "credentialID" text NOT NULL,
    counter integer NOT NULL,
    "deviceType" text NOT NULL,
    "backedUp" integer NOT NULL,
    transports text,
    "createdAt" date,
    aaguid text
);
CREATE INDEX IF NOT EXISTS passkey_userId_idx ON passkey ("userId");
CREATE INDEX IF NOT EXISTS passkey_credentialID_idx ON passkey ("credentialID");
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0013AuthTwoFactorPasskeys {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("user", "twoFactorEnabled").await? {
            manager
                .get_connection()
                .execute_unprepared(r#"ALTER TABLE "user" ADD COLUMN "twoFactorEnabled" integer;"#)
                .await?;
        }
        manager
            .get_connection()
            .execute_unprepared(M0013_UP)
            .await?;
        Ok(())
    }
}

struct M0014DownloadsAndBlocklist;

impl MigrationName for M0014DownloadsAndBlocklist {
    fn name(&self) -> &str {
        "m0014_downloads_and_blocklist"
    }
}

/// Active and historical download tracking plus release blocklist
/// (docs/04-database.md, "Search and download tables").
const M0014_UP: &str = r#"
CREATE TABLE downloads (
    id integer PRIMARY KEY AUTOINCREMENT,
    movie_id integer NOT NULL REFERENCES movies (id) ON DELETE CASCADE,
    movie_profile_id integer REFERENCES movie_profiles (id) ON DELETE SET NULL,
    indexer_id integer REFERENCES indexers (id) ON DELETE SET NULL,
    download_client_id integer REFERENCES download_clients (id) ON DELETE SET NULL,
    client_download_id text NOT NULL,
    release_title text NOT NULL,
    quality_id integer NOT NULL,
    custom_format_score integer NOT NULL DEFAULT 0,
    size integer NOT NULL,
    state text NOT NULL,
    error text,
    attempts integer NOT NULL DEFAULT 0,
    grabbed_at text NOT NULL,
    completed_at text,
    imported_at text,
    UNIQUE (download_client_id, client_download_id)
);
CREATE INDEX downloads_state_idx ON downloads (state);
CREATE INDEX downloads_movie_id_idx ON downloads (movie_id);

CREATE TABLE blocklist (
    id integer PRIMARY KEY AUTOINCREMENT,
    movie_id integer NOT NULL REFERENCES movies (id) ON DELETE CASCADE,
    release_title text NOT NULL,
    indexer_id integer REFERENCES indexers (id) ON DELETE SET NULL,
    reason text NOT NULL,
    created_at text NOT NULL
);
CREATE INDEX blocklist_movie_id_idx ON blocklist (movie_id);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0014DownloadsAndBlocklist {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0014_UP)
            .await?;
        Ok(())
    }
}

struct M0015Libraries;

impl MigrationName for M0015Libraries {
    fn name(&self) -> &str {
        "m0015_libraries"
    }
}

/// Libraries as named groups of root folders and per-library access scoping
/// (docs/04-database.md, docs/15-permissions.md, ADR 0012).
///
/// Seeds one `Default` library and puts every existing root folder into it.
/// Existing role assignments and direct grants keep `all_libraries = TRUE`, so
/// upgraded installs behave identically.
const M0015_UP: &str = r#"
PRAGMA foreign_keys = OFF;
BEGIN;

CREATE TABLE libraries (
    id integer PRIMARY KEY AUTOINCREMENT,
    name text NOT NULL UNIQUE,
    created_at text NOT NULL,
    updated_at text NOT NULL
);

INSERT INTO libraries (name, created_at, updated_at)
VALUES ('Default',
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

CREATE TABLE root_folders_new (
    id integer PRIMARY KEY AUTOINCREMENT,
    path text NOT NULL UNIQUE,
    library_id integer NOT NULL DEFAULT 1 REFERENCES libraries (id) ON DELETE RESTRICT,
    created_at text NOT NULL
);
INSERT INTO root_folders_new (id, path, library_id, created_at)
SELECT id, path, 1, created_at FROM root_folders;
DROP TABLE root_folders;
ALTER TABLE root_folders_new RENAME TO root_folders;
CREATE INDEX root_folders_library_id_idx ON root_folders (library_id);

ALTER TABLE user_roles ADD COLUMN all_libraries boolean NOT NULL DEFAULT TRUE;

CREATE TABLE user_role_libraries (
    user_id text NOT NULL,
    role_id integer NOT NULL,
    library_id integer NOT NULL REFERENCES libraries (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role_id, library_id),
    FOREIGN KEY (user_id, role_id) REFERENCES user_roles (user_id, role_id) ON DELETE CASCADE
);
CREATE INDEX user_role_libraries_library_id_idx ON user_role_libraries (library_id);

ALTER TABLE user_permissions ADD COLUMN all_libraries boolean NOT NULL DEFAULT TRUE;

CREATE TABLE user_permission_libraries (
    user_id text NOT NULL,
    permission text NOT NULL,
    library_id integer NOT NULL REFERENCES libraries (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, permission, library_id),
    FOREIGN KEY (user_id, permission) REFERENCES user_permissions (user_id, permission) ON DELETE CASCADE
);
CREATE INDEX user_permission_libraries_library_id_idx ON user_permission_libraries (library_id);

COMMIT;
PRAGMA foreign_keys = ON;
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0015Libraries {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0015_UP)
            .await?;
        Ok(())
    }
}

struct M0016TvShows;

impl MigrationName for M0016TvShows {
    fn name(&self) -> &str {
        "m0016_tv_shows"
    }
}

/// TV tables and the episode side of `downloads` (docs/04-database.md,
/// "TV tables (M7)"). The `downloads` rebuild makes `movie_id` nullable, so
/// the table must be recreated to add the check that exactly one target is set.
const M0016_UP: &str = r#"
PRAGMA foreign_keys = OFF;
BEGIN;

CREATE TABLE shows (
    id integer PRIMARY KEY AUTOINCREMENT,
    tvdb_id integer UNIQUE,
    tmdb_id integer UNIQUE,
    imdb_id text,
    metadata_provider text NOT NULL CHECK (metadata_provider IN ('tvdb', 'tmdb')),
    title text NOT NULL,
    original_title text,
    sort_title text NOT NULL,
    year integer NOT NULL,
    overview text,
    runtime integer,
    status text NOT NULL CHECK (status IN ('continuing', 'ended', 'upcoming')),
    series_type text NOT NULL CHECK (series_type IN ('standard', 'daily', 'anime')),
    network text,
    poster_path text,
    season_folder boolean NOT NULL DEFAULT TRUE,
    root_folder_id integer NOT NULL REFERENCES root_folders (id),
    folder_name text NOT NULL,
    monitored boolean NOT NULL,
    monitor_new_items text NOT NULL DEFAULT 'all' CHECK (monitor_new_items IN ('all', 'none')),
    refreshed_at text,
    added_at text NOT NULL,
    updated_at text NOT NULL,
    CHECK (tvdb_id IS NOT NULL OR tmdb_id IS NOT NULL)
);
CREATE INDEX shows_root_folder_id_idx ON shows (root_folder_id);

CREATE TABLE seasons (
    show_id integer NOT NULL REFERENCES shows (id) ON DELETE CASCADE,
    season_number integer NOT NULL,
    monitored boolean NOT NULL,
    PRIMARY KEY (show_id, season_number)
);

CREATE TABLE episodes (
    id integer PRIMARY KEY AUTOINCREMENT,
    show_id integer NOT NULL,
    season_number integer NOT NULL,
    episode_number integer NOT NULL,
    absolute_number integer,
    title text NOT NULL,
    overview text,
    air_date text,
    air_date_utc text,
    runtime integer,
    tvdb_id integer UNIQUE,
    tmdb_id integer UNIQUE,
    monitored boolean NOT NULL,
    added_at text NOT NULL,
    updated_at text NOT NULL,
    UNIQUE (show_id, season_number, episode_number),
    FOREIGN KEY (show_id, season_number) REFERENCES seasons (show_id, season_number) ON DELETE CASCADE
);
CREATE INDEX episodes_air_date_idx ON episodes (air_date);
CREATE INDEX episodes_show_id_absolute_number_idx ON episodes (show_id, absolute_number);

CREATE TABLE episode_files (
    id integer PRIMARY KEY AUTOINCREMENT,
    show_id integer NOT NULL REFERENCES shows (id) ON DELETE CASCADE,
    relative_path text NOT NULL,
    size integer NOT NULL,
    quality_id integer NOT NULL,
    release_group text,
    scene_name text,
    media_info json,
    languages json,
    added_at text NOT NULL,
    UNIQUE (show_id, relative_path)
);

CREATE TABLE episode_file_episodes (
    episode_file_id integer NOT NULL REFERENCES episode_files (id) ON DELETE CASCADE,
    episode_id integer NOT NULL REFERENCES episodes (id) ON DELETE CASCADE,
    PRIMARY KEY (episode_file_id, episode_id)
);
CREATE INDEX episode_file_episodes_episode_id_idx ON episode_file_episodes (episode_id);

CREATE TABLE downloads_new (
    id integer PRIMARY KEY AUTOINCREMENT,
    movie_id integer REFERENCES movies (id) ON DELETE CASCADE,
    show_id integer REFERENCES shows (id) ON DELETE CASCADE,
    movie_profile_id integer REFERENCES movie_profiles (id) ON DELETE SET NULL,
    indexer_id integer REFERENCES indexers (id) ON DELETE SET NULL,
    download_client_id integer REFERENCES download_clients (id) ON DELETE SET NULL,
    client_download_id text NOT NULL,
    release_title text NOT NULL,
    quality_id integer NOT NULL,
    custom_format_score integer NOT NULL DEFAULT 0,
    size integer NOT NULL,
    state text NOT NULL,
    error text,
    attempts integer NOT NULL DEFAULT 0,
    grabbed_at text NOT NULL,
    completed_at text,
    imported_at text,
    UNIQUE (download_client_id, client_download_id),
    CHECK ((movie_id IS NOT NULL AND show_id IS NULL)
        OR (movie_id IS NULL AND show_id IS NOT NULL))
);
INSERT INTO downloads_new (id, movie_id, movie_profile_id, indexer_id, download_client_id,
    client_download_id, release_title, quality_id, custom_format_score, size, state, error,
    attempts, grabbed_at, completed_at, imported_at)
SELECT id, movie_id, movie_profile_id, indexer_id, download_client_id,
    client_download_id, release_title, quality_id, custom_format_score, size, state, error,
    attempts, grabbed_at, completed_at, imported_at
FROM downloads;
DROP TABLE downloads;
ALTER TABLE downloads_new RENAME TO downloads;
CREATE INDEX downloads_state_idx ON downloads (state);
CREATE INDEX downloads_movie_id_idx ON downloads (movie_id);
CREATE INDEX downloads_show_id_idx ON downloads (show_id);

CREATE TABLE download_episodes (
    download_id integer NOT NULL REFERENCES downloads (id) ON DELETE CASCADE,
    episode_id integer NOT NULL REFERENCES episodes (id) ON DELETE CASCADE,
    PRIMARY KEY (download_id, episode_id)
);
CREATE INDEX download_episodes_episode_id_idx ON download_episodes (episode_id);

COMMIT;
PRAGMA foreign_keys = ON;
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0016TvShows {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if let Err(error) = db.execute_unprepared(M0016_UP).await {
            // The script switches foreign keys off and opens a transaction: undo both
            // so the connection goes back to the pool in its normal state.
            let _ = db.execute_unprepared("ROLLBACK").await;
            let _ = db.execute_unprepared("PRAGMA foreign_keys = ON").await;
            return Err(error);
        }
        let violations = db
            .query_all_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "PRAGMA foreign_key_check",
            ))
            .await?;
        if !violations.is_empty() {
            return Err(DbErr::Migration(format!(
                "m0016_tv_shows left {} foreign key violations",
                violations.len()
            )));
        }
        Ok(())
    }
}

struct M0017History;

impl MigrationName for M0017History {
    fn name(&self) -> &str {
        "m0017_history"
    }
}

/// The activity trail (docs/04-database.md, "Core tables"): one row per event,
/// with the event specific payload as JSON.
const M0017_UP: &str = r#"
CREATE TABLE history (
    id integer PRIMARY KEY AUTOINCREMENT,
    movie_id integer REFERENCES movies (id) ON DELETE SET NULL,
    event text NOT NULL,
    data text NOT NULL,
    created_at text NOT NULL
);
CREATE INDEX history_movie_id_created_at_idx ON history (movie_id, created_at);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0017History {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0017_UP)
            .await?;
        Ok(())
    }
}

struct M0018ImportSources;

impl MigrationName for M0018ImportSources {
    fn name(&self) -> &str {
        "m0018_import_sources"
    }
}

/// Import sources and the map that makes a run safe to repeat
/// (docs/04-database.md, "System tables"). A path mapping translates the
/// source's paths, like a download client's remote path mapping.
const M0018_UP: &str = r#"
CREATE TABLE import_sources (
    id integer PRIMARY KEY AUTOINCREMENT,
    kind text NOT NULL CHECK (kind IN ('radarr')),
    name text NOT NULL,
    url text NOT NULL UNIQUE,
    api_key text NOT NULL,
    target_quality_profile_id integer REFERENCES quality_profiles (id) ON DELETE SET NULL,
    remote_path text,
    local_path text,
    last_import_at text,
    created_at text NOT NULL,
    updated_at text NOT NULL,
    CHECK ((remote_path IS NULL) = (local_path IS NULL))
);

CREATE TABLE import_map (
    source_id integer NOT NULL REFERENCES import_sources (id) ON DELETE CASCADE,
    resource text NOT NULL,
    source_resource_id integer NOT NULL,
    local_id integer NOT NULL,
    PRIMARY KEY (source_id, resource, source_resource_id)
);
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0018ImportSources {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0018_UP)
            .await?;
        Ok(())
    }
}

struct M0019ImportSourceOrderAndOptions;

impl MigrationName for M0019ImportSourceOrderAndOptions {
    fn name(&self) -> &str {
        "m0019_import_source_order_and_options"
    }
}

/// The position orders the sources, `resources` is a JSON array of the
/// groups to import (NULL means all), and `import_quality_definitions`
/// applies the source's global size limits.
const M0019_UP: &str = r#"
ALTER TABLE import_sources ADD COLUMN position integer NOT NULL DEFAULT 0;
ALTER TABLE import_sources ADD COLUMN resources text;
ALTER TABLE import_sources ADD COLUMN import_quality_definitions integer NOT NULL DEFAULT 0;
UPDATE import_sources SET position = id;
"#;

#[async_trait::async_trait]
impl MigrationTrait for M0019ImportSourceOrderAndOptions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(M0019_UP)
            .await?;
        Ok(())
    }
}
