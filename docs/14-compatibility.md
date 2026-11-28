# 14 Sonarr/Radarr compatibility

Status: reviewed (2026-11-16). Scope decision: ADR 0008.

"Compatible" means the four layers below. Anything not listed is not promised.

## A. On disk

| Item | V1 (movies) | Later |
|---|---|---|
| Adopt an existing library in place | Radarr | Sonarr (M8) |
| Default naming equals upstream defaults | Yes | TV |
| Template tokens accepted | See [09-filesystem.md](09-filesystem.md) | TV tokens |
| Names rendered from a template | Same as Radarr, except `{Release Group}` without a group (`Unseenarr`, not `Radarr`), Windows-unsafe characters and device names (replaced on every platform), and the `:` token options (refused) | TV |
| Hardlink behaviour | Same | Same |
| qBittorrent category convention | Category per client, reused on import | Same |

## B. Concepts and names

V1: quality names and ids, quality definitions, quality profiles with
cutoff, upgrades, language and custom format scores, custom formats in
Radarr's JSON format, root folders, tags (also on indexers and download
clients), collections, monitored, minimum availability. Release parsing
agrees with Radarr on the table in
[07-release-scoring.md](07-release-scoring.md).

TRaSH guides: built in, V1 ([07-release-scoring.md](07-release-scoring.md)).
Recyclarr: after V1 (M11). It needs compat write endpoints for custom
formats, quality profiles and quality definitions.

## C. Import from running instances

Read-only against the source. Safe to re-run (`import_map` table).

| Radarr resource | Endpoint | Becomes in Unseenarr |
|---|---|---|
| Root folders | `GET /rootfolder` | Root folder with the same path (path mapping if mounts differ) |
| Quality profiles | `GET /qualityprofile` | Quality profile with language, custom format scores and score limits |
| Custom formats | `GET /customformat` | Custom format, matched by name; the JSON is stored unchanged |
| Quality definitions | `GET /qualitydefinition` | Size limits per quality; applied only when the user ticks it, because the table is global |
| Collections | `GET /collection` | Collection matched by `tmdbId`, with its monitored state |
| Tags | `GET /tag` | Tag, matched by label |
| Movies | `GET /movie` | Movie matched by `tmdbId`, with path, monitored, minimum availability, tags; existing file adopted in place |
| Indexers | `GET /indexer` | Torznab indexers only; secrets are masked upstream, so entries arrive disabled |
| Download clients | `GET /downloadclient` | qBittorrent only; password must be re-entered |

Several instances: each import source has a target quality profile. All
movies from that source get that profile, or keep their mapped profile if no
target is set.

The page is `/import/radarr`: the saved sources with their preview and re-run,
the ↑/↓ order, a per-source list of resource groups, the quality-sizes switch
and the report. `POST /import-sources/test` checks a connection without saving
it, `POST /import-sources/{id}/preview` counts a run without writing, and
`PATCH /import-sources/order` stores the source order.

A movie present in two instances is merged by `tmdbId`, and nothing is lost:

- The movie gets one movie profile per instance, each with the file that
  instance had. This is the 1080p-and-4K case that needed two instances.
- The first imported source provides the primary profile, the root folder,
  the folder name and the monitored state. The import page lets the user
  order the sources.
- Tags are the union. Minimum availability is the earliest of the two.
- If both instances map to the same quality profile, the movie gets one
  movie profile; the larger file becomes its file and the other is adopted
  as an unmanaged file.
- If the two instances keep the movie in different folders, the file of the
  second one stays where it is and is recorded with its real path. The
  report lists these movies.

Custom formats with the same name and different conditions in two instances:
the first source wins, the second is imported as `<name> (<source name>)`
and reported.

Later: Sonarr (M8), Prowlarr indexers through its v1 API (M11). Bazarr
settings: not in V1, decided in the subtitles design (M10).

## D. API for companion tools

Base: `/radarr/api/v3`. Auth: `X-Api-Key` header or `?apikey=` query, backed
by Unseenarr API keys (session cookies are not accepted on the compat API). Paths
are matched case-insensitively. Fields without a value in Unseenarr are returned
with a default, never omitted. Unimplemented endpoints (after API-key
authentication) return `501` with `{ "message": "Not implemented", "method": ..., "path": ... }`
and log one `warn` entry with method and path. Errors use Radarr-style JSON
bodies (`application/json`): `401` returns `{ "error": "Unauthorized" }`,
`403` returns `{ "message": "Forbidden", "description": ..., "requiredPermission": ... }`,
`404` returns `{ "message": "NotFound" }`, and `400` validation errors return
an array of `{ propertyName, errorMessage, attemptedValue, severity, errorCode }`.

V1 client: Seerr. Endpoint list taken from Seerr's source,
`seerr-team/seerr` branch `develop` at commit `fc53b3d` (2026-11-15):
`server/api/servarr/base.ts`, `server/api/servarr/radarr.ts`.

| Method and path | Status | Client | Permission |
|---|---|---|---|
| `GET /system/status` | V1 | Seerr connection test | `settings:read` |
| `GET /qualityprofile` | V1 | Seerr | `profiles:read` |
| `GET /rootfolder` | V1 | Seerr | `settings:read` |
| `GET /tag` | V1 | Seerr | `library:read` |
| `POST /tag` | V1 | Seerr (tag per requester) | `library:edit` |
| `PUT /tag/{id}` | V1 | Seerr | `library:edit` |
| `GET /movie` | V1 | Seerr library scan | `library:read` |
| `GET /movie?tmdbId=` | V1 | Seerr | `library:read` |
| `GET /movie/{id}` | V1 | Seerr availability sync | `library:read` |
| `GET /movie/lookup?term=tmdb:{id}` | V1 | Seerr, before every add | `library:read` |
| `POST /movie` | V1 | Seerr request | `library:add` |
| `PUT /movie` | V1 | Seerr re-monitor | `library:edit` |
| `DELETE /movie/{id}` | V1 | Seerr remove | `library:delete` |
| `POST /command` (`MoviesSearch`) | V1 | Seerr | `search:run` |
| `POST /command` (`RefreshMonitoredDownloads`) | V1 | Seerr download tracker | `queue:read` |
| `GET /queue` | V1 | Seerr download tracker | `queue:read` |
| `GET /health`, `/diskspace`, `/calendar`, `/wanted/missing` | Later | Homarr, Homepage | to define |
| `GET /history`, `/movie/lookup?term=<text>` | Later | Dashboards, other request tools | to define |
| `GET/POST/PUT/DELETE /customformat`, `PUT /qualityprofile/{id}`, `GET/PUT /qualitydefinition` | M11 | Recyclarr | `profiles:read`, `profiles:manage` |
| `/notification`, `/config/*`, `/indexer`, `/downloadclient` writes | Never | | |
| `/sonarr/api/v3/*` | M8 | Seerr | See the Sonarr table below |

`POST /movie` with `addOptions.searchForMovie = true` also needs
`search:run`. Open question: one route, one permission is the rule; draft
answer is that the add succeeds and the search is skipped with a log line
when the key lacks `search:run`.

### Sonarr endpoints (M8)

Base: `/sonarr/api/v3`, same auth and rules as above. Endpoint list taken from
Seerr's source, `seerr-team/seerr` branch `develop` at commit `3f3fee5`
(2026-11-23): `server/api/servarr/base.ts` and `server/api/servarr/sonarr.ts`.
Every call Seerr makes to Sonarr is in the table. Paths shared with Radarr
(`base.ts`) come first.

| Method and path | Status | Client | Permission |
|---|---|---|---|
| `GET /system/status` | M8 | Seerr connection test | `settings:read` |
| `GET /qualityProfile` | M8 | Seerr | `profiles:read` |
| `GET /rootfolder` | M8 | Seerr | `settings:read` |
| `GET /tag` | M8 | Seerr | `library:read` |
| `POST /tag` | M8 | Seerr (tag per requester) | `library:edit` |
| `PUT /tag/{id}` | M8 | Seerr | `library:edit` |
| `GET /queue?includeEpisode=true` | M8 | Seerr download tracker | `queue:read` |
| `POST /command` (`RefreshMonitoredDownloads`) | M8 | Seerr download tracker | `queue:read` |
| `GET /series` | M8 | Seerr library scan | `library:read` |
| `GET /series?tvdbId=` | M8 | Seerr | `library:read` |
| `GET /series/{id}` | M8 | Seerr availability sync | `library:read` |
| `GET /series/lookup?term=tvdb:{id}` | M8 | Seerr, before every add | `library:read` |
| `GET /series/lookup?term=<title>` | M8 | Seerr | `library:read` |
| `POST /series` | M8 | Seerr request | `library:add` |
| `PUT /series` | M8 | Seerr, request for a series already in the library | `library:edit` |
| `DELETE /series/{id}?deleteFiles=true&addImportExclusion=false` | M8 | Seerr remove | `library:delete` |
| `GET /episode?seriesId=` | M8 | Seerr, after a series update | `library:read` |
| `PUT /episode/monitor` | M8 | Seerr, re-monitor requested seasons | `library:edit` |
| `POST /command` (`MissingEpisodeSearch`, `seriesId`) | M8 | Seerr | `search:run` |
| `GET /languageprofile` | Stub | Seerr settings page | `profiles:read` |

Notes:

- Seerr asks for `/qualityProfile` with that capitalisation. Paths are
  case-insensitive here, so one route serves it.
- `POST /series` with `addOptions.searchForMissingEpisodes = true` also needs
  `search:run`. Same open question and draft answer as `POST /movie` above.
- Sonarr v4 has no language profiles and Seerr tolerates the failure of
  `GET /languageprofile`. Draft answer: return an empty list.
- Seerr sends `monitorNewItems` and `seriesType` (`standard`, `daily`,
  `anime`) on `POST /series`. The first needs a column on the show
  (#88).
- `/series` with a `PUT` body carries the whole resource. Seerr changes only
  `monitored`, `tags` and the season list; Unseenarr applies those three.
- The pinned Sonarr OpenAPI document is `v5.0.0.3041` (commit `03f822c`,
  `src/Sonarr.Api.V3/openapi.json`), stored as
  `fixtures/sonarr/openapi-5.0.0.3041.json`. `SONARR_VERSION` in
  `arr-compat::sonarr` matches that version, DTOs live in
  `arr-compat::sonarr::dto`, mapping in `arr-compat::sonarr::map`, and
  `crates/arr-compat/tests/sonarr_schema.rs` checks every serialised resource
  against the pinned schema (with `EpisodeResource.episodeFile`,
  `EpisodeResource.series`, `QueueResource.series`, `QueueResource.episode`,
  and `QualityProfileQualityItemResource.quality` allowed as `null` when
  absent). Quality ids translate bidirectionally across the four differing TV
  qualities listed in [07-release-scoring.md](07-release-scoring.md).

## Field mapping: Radarr MovieResource

| Radarr field | Unseenarr source |
|---|---|
| `id` | `movies.id` |
| `title`, `originalTitle`, `sortTitle`, `year`, `overview`, `runtime` | same columns |
| `tmdbId`, `imdbId` | `tmdb_id`, `imdb_id` |
| `titleSlug` | TMDB id as string |
| `path` | root folder path + `folder_name` |
| `rootFolderPath` | root folder path |
| `folderName` | same as `path` |
| `qualityProfileId` | `quality_profile_id` |
| `monitored`, `minimumAvailability` | same columns |
| `isAvailable` | computed from release dates and minimum availability |
| `hasFile`, `movieFile` | from `movie_files`; `movieFile.mediaInfo` defaults when unknown |
| `status` | `tba`, `announced`, `inCinemas`, `released` from TMDB dates |
| `added` | `added_at` |
| `tags` | tag ids |
| `images`, `ratings`, `genres`, `studio`, `certification`, `youTubeTrailerId`, `website`, `collection`, `popularity`, `statistics` | value if stored, otherwise the schema default (empty list, empty string, zero) |

`system/status` returns `appName: "Radarr"`, `urlBase: "/radarr"` and a fixed
Radarr version: the version of the Radarr release whose OpenAPI document and
recorded responses are pinned for the contract tests. One constant in
`arr-compat`, raised together with the pinned schema. A client that reads
the version then sees exactly the API it was told about. Unseenarr's own
version is in `GET /api/v1/health`.

### Pinned schema and DTOs

The pinned OpenAPI document is Radarr `v7.0.3.10918` (commit `af772b0`,
`src/Radarr.Api.V3/openapi.json`), stored as
`fixtures/radarr/openapi-7.0.3.10918.json`. `RADARR_VERSION` in
`arr-compat` is that version; a test fails if the file name and the constant
differ. The DTOs are in `arr-compat::radarr::dto`, the mapping in
`arr-compat::radarr::map`. `arr-compat` does not read the database, so the
mapping takes plain `*View` structs that `api` fills from the `db` models.

Rules the contract test (`crates/arr-compat/tests/schema.rs`) checks:

- Every property of the schema is in the output, none is added. The upstream
  document has no `required` lists; the test demands all properties anyway.
- A value Unseenarr lacks is the default: empty string, zero, empty list,
  `false`, `0001-01-01T00:00:00Z` for a date-time that cannot be null, and
  `null` where the schema is `nullable` (for example `inCinemas`, `hasFile`).
- Four objects are `null` instead of a made-up default: `movieFile` of a
  movie without a file, `collection`, `movie` of a queue record without a
  movie, and `quality` of a quality group. Radarr leaves them out; Unseenarr
  sends `null` so that the key is present. The schema does not mark them
  nullable; the test lists them.
- Quality profile `language` uses Radarr's language ids (`any` is -1,
  `original` is -2, `en` is 1; a code Unseenarr cannot place is 0, `Unknown`).
- `minimumAvailability` on input: `tba` means `announced`, `deleted` is
  rejected. Missing means `released`, as in Unseenarr's own API.
- Input bodies are read leniently: unknown fields are ignored and every
  field is optional until an operation needs it (`MovieRequest::into_add`
  needs `tmdbId`, `qualityProfileId` and `rootFolderPath`; `into_update`
  needs `id`). `movieIds` of `POST /command` is not in the upstream `Command`
  schema and is read from its own struct.

`qualityProfileId`, `hasFile` and `movieFile` describe the primary movie
profile. Other profiles and unmanaged files are not visible through the
compat API in V1.

Quality profile, root folder, tag and queue resources: full mapping tables
are written with their M6 issues.

## Known differences

- One Unseenarr serves what used to be several instances. A companion tool that
  needs "4K server" and "1080p server" entries points both at Unseenarr and
  chooses a different quality profile.
- Ids differ from the imported instances.
- A movie can have several profiles and files; the compat API shows the
  primary one.
- Custom formats exist, but the compat API does not expose them until M11;
  related fields return empty lists and zero scores until then.
- Commands other than the two listed return 501.
- `queue` records carry no custom format data until M11.

## Decisions (2026-11-15)

- Custom formats: V1. TRaSH guides: V1, built in. Recyclarr: after V1.
- Merge rule for a movie in two instances: both kept as movie profiles, see
  section C.
- `system/status` reports the pinned Radarr version.
- Bazarr settings import: not in V1.

## Open questions

- Seerr with a "4K server" entry that points at Unseenarr: what must `GET
  /movie?tmdbId=` and `POST /movie` answer so that a 4K request adds a second
  movie profile? Verify with Seerr in M6.
