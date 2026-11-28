# 08 External integration contracts

Status: reviewed (2026-11-16). Verify every endpoint against the provider's current
documentation when its issue is implemented.

All clients: one shared `reqwest` client, timeouts (10 s connect, 30 s total),
a user agent `Unseenarr/<version>`, and typed errors that map to HTTP 502 in our
API. Every user-entered URL passes the outbound URL check in
[11-security.md](11-security.md). Tests use recorded fixtures served by a local mock HTTP server
(`wiremock`). No test calls the real service.

## Metadata providers

`core` defines a metadata provider trait: search, lookup by id, details,
collection details. TMDB is the V1 implementation. TVDB follows in M7. The
rest of the code depends on the trait only.

## TMDB (M2)

| | |
|---|---|
| Endpoints | `GET /3/search/movie`, `GET /3/movie/{id}?append_to_response=external_ids,release_dates`, `GET /3/find/{imdb_id}`, `GET /3/collection/{id}`, `GET /3/configuration` |
| Auth | API key or bearer token set by the Owner in settings |
| Rate limits | About 40 requests per second per IP; back off on 429 with `Retry-After` |
| Failure | Add fails with 502. Refresh keeps the stored metadata. |
| Mock | JSON fixtures per endpoint |

Open question: ship a shared TMDB key, or require the Owner's own key.

## Torznab (M3)

| | |
|---|---|
| Endpoints | `?t=caps`, `?t=movie&imdbid=&tmdbid=&q=`, `?t=search&q=`, RSS: `?t=movie` without query |
| Auth | `apikey` query parameter |
| Rate limits | Per indexer; configurable minimum delay; back off on 429 and 5xx |
| Failure | Skip the indexer for this search, record the error, disable temporarily after repeated failures with increasing back-off |
| Mock | XML fixtures: caps, results with `torznab:attr`, error response |

Use only the search modes and parameters that `caps` advertises.

Proxy: one optional proxy (HTTP, HTTPS or SOCKS5, with optional credentials)
in settings. It applies to indexer traffic only. Each indexer has a "use
proxy" switch, default on. Local addresses bypass the proxy by default.
TMDB, download clients and import sources are never proxied.

Details: the URL is `http://`, `https://`, `socks5://` or `socks5h://`, with
optional `user:password@`, stored encrypted in `proxy.url` and never returned
(the API shows the address without credentials). SOCKS5 always lets the proxy
resolve names. "Local" for the bypass means a private, loopback or
link-local address, `localhost`, a name without a dot (a container name) and
the suffixes `.local`, `.lan`, `.internal` and `.home.arpa`. The system proxy
variables (`HTTP_PROXY` and others) are ignored for indexer traffic. When a
proxy is set but cannot be used (unreadable or invalid), an indexer with the
switch on fails; it never falls back to a direct connection. Through a proxy
the target name is resolved by the proxy, so the check on resolved addresses
does not apply to it; the check on the URL itself still does.

## TRaSH guides (M5)

| | |
|---|---|
| Source | The JSON files in the public TRaSH-Guides GitHub repository (Radarr custom formats, custom format groups, quality profiles, quality sizes) |
| Auth | None |
| Rate limits | One download of the repository archive per sync; default once a day; conditional request so an unchanged guide costs nothing |
| Failure | Keep the stored copy; report the error on the task |
| Mock | A small copy of the guide folder as fixtures |

## Newznab (M9)

| | |
|---|---|
| Endpoints | `?t=caps`, `?t=movie&imdbid=&tmdbid=&q=&extended=1`, `?t=search&q=&extended=1`, RSS: `?t=movie&extended=1` without query |
| Auth | `apikey` query parameter |
| Rate limits | Per indexer; back off on HTTP 429 and XML error codes 429, 500 and 501 (honoring `Retry-After` when present) |
| Failure | Skip the indexer for this search; XML codes 100–109 or HTTP 401/403 map to `ApiKey` |
| Mock | XML fixtures from two indexers (NZBgeek and DrunkenSlug): caps, movie results with NZB enclosures and `newznab:attr`, credential and rate-limit errors |

Implementation notes (`newznab.rs`):

- Same capability-first rules as Torznab: use only modes and parameters `caps`
  advertises, preferring `imdbid` (without `tt`), then `tmdbid`, then title
  search. Passes `extended=1` so indexers include `newznab:attr` metadata.
- Picks the `application/x-nzb` `<enclosure>` (falling back to an untyped
  enclosure, then `<link>`) and ignores non-NZB attachments such as cover
  images. Items without an NZB address are skipped.
- Parses `newznab:attr` fields (`size`, `category`, `grabs`, `usenetdate`,
  `imdb`, `tmdbid`, `files`, `poster`, `group`, `password`) and computes
  `age_hours` / `age_days` from `usenetdate` (falling back to `pubDate`).
- Indexer rows accept `kind = "newznab"` alongside `"torznab"`, share the same
  write-only encrypted `api_key` and outbound proxy rules, and validate via
  `t=caps` followed by an RSS fetch with `limit=1`.

## qBittorrent (M4)

| | |
|---|---|
| Endpoints (WebUI API v2) | `POST /api/v2/auth/login`, `GET /api/v2/app/version`, `POST /api/v2/torrents/add`, `GET /api/v2/torrents/info?category=`, `GET /api/v2/torrents/properties`, `POST /api/v2/torrents/delete`, `GET /api/v2/torrents/categories`, `POST /api/v2/torrents/createCategory`, `GET /api/v2/app/preferences` |
| Auth | API key (qBittorrent 5.2 or later): `Authorization: Bearer qbt_...` on every request, no login. Otherwise a cookie session from username and password; log in again on 403. When a client has a key, the key wins |
| Rate limits | None; poll every 5 s while downloads are active, 60 s when idle |
| Failure | See retries in [06-state-machines.md](06-state-machines.md) |
| Mock | JSON fixtures; one integration test may use a real qBittorrent container (not in default CI) |

Torrents are identified by info hash. The category is how Unseenarr finds its
own torrents.

Rules of the client in `core` (`qbittorrent.rs`):

- Every torrent call takes the category of the download client and refuses an
  empty one, because an empty category filter means the torrents that have no
  category. A torrent in another category, or in none, is never listed. A
  delete or a properties call for its hash does nothing: the hashes sent to
  qBittorrent are the ones found in our category, never the caller's text.
- Add takes a magnet link or an http(s) torrent URL, sets the category and
  nothing else. The save path is left to qBittorrent, and Unseenarr reads
  `content_path` from the torrent list. The category is created when
  `torrents/categories` does not have it, without a save path. A magnet link
  gives its info hash at once; a torrent URL does not, because qBittorrent
  fetches it, so the caller finds that torrent by listing the category.
- The session cookie is kept between calls. A 403 on a call logs in again once
  and repeats the call; a second 403 is `Credentials`. An API key never logs in.

## SABnzbd (M9)

| | |
|---|---|
| Endpoints (`/api?output=json&apikey=`) | `mode=queue`, `mode=history`, `mode=addurl`, `mode=version` |
| Auth | API key in the `apikey` query parameter on every call |
| Rate limits | None; poll while downloads are active, 60 s when idle |
| Failure | An HTTP 401 names the key; anything else is unavailable |
| Mock | JSON fixtures per endpoint |

- Add takes an http(s) NZB URL as `name` and the category as `cat`, with
  `nzbname` only when a display name is given, and returns the first of
  `nzo_ids`.
- Queue and history take `cat` and keep only the slots of that category.
- Delete finds our ids first and deletes queue ids with
  `mode=queue&name=delete&value=&del_files=`, history ids with
  `mode=history&name=delete&value=&archive=0&del_files=` (`archive=0` fully
  removes the slot; the default archives). `del_files=1` when files go too.
- A wrong key is HTTP 401.

## Subtitle providers

`core` defines a subtitle provider trait; OpenSubtitles is the only V1
implementation and other providers are out of scope. The rest of the code
depends on the trait only.

```rust
trait SubtitleProvider: Send + Sync {
    /// Candidates for one movie, best provider order first. No quota used.
    async fn search(&self, query: &SubtitleQuery) -> Result<Vec<SubtitleCandidate>, SubtitleError>;
    /// The subtitle file of one candidate. Uses one unit of quota.
    async fn download(&self, id: &str) -> Result<DownloadedSubtitle, SubtitleError>;
}
```

- `SubtitleQuery`: TMDB id, IMDb id, title, year, the languages and kinds
  wanted, the 64-bit OpenSubtitles hash of the video file, and the file name.
- `SubtitleCandidate`: the provider's id, language, kind, release name,
  whether the hash matched, whether the uploader is trusted, download count,
  rating, and whether it is machine or AI translated.
- `DownloadedSubtitle`: the bytes, the file extension, and the quota left
  and when it resets, when the provider says so.
- `SubtitleError`: `NotFound`, `Unauthorized` (bad API key or login),
  `QuotaExhausted { resets_at }`, `RateLimited` (still limited after the
  retries) and `Unavailable(String)`. They map to HTTP 502, except
  `QuotaExhausted`, which maps to 429 with the reset time.

Scoring of candidates (below) is in `core`, not in the provider, so that a
second provider needs no new scoring code.

## OpenSubtitles (M10)

| | |
|---|---|
| Endpoints (REST API v1, `https://api.opensubtitles.com/api/v1`) | `POST /login`, `GET /subtitles`, `POST /download` |
| Auth | A consumer API key in the `Api-Key` header on every request, and the bearer token that `/login` returns for downloads. The user enters their own API key, username and password in settings (stored encrypted). Unseenarr ships no shared key. |
| Search | By `tmdb_id`, `languages`, `moviehash`, `hearing_impaired` and `foreign_parts_only`. A search uses no download quota. |
| Download quota | Each `POST /download` uses one unit. The answer carries the units left and the reset time; Unseenarr keeps them and never hard-codes a number, because the quota depends on the account level and the provider changes it. At zero, downloads stop until the reset time and the next scheduled run tries again. A 429 on a download is treated the same way. |
| Rate limits | Search and download calls go through one limiter, a minimum delay between calls (default 1 s). On 429, honor `Retry-After`, else back off. |
| Failure | A failed search leaves the subtitle wanted and records the error on the task. A bad API key or login disables the provider and shows an error in settings until it is changed. |
| Mock | JSON fixtures: search results, a login answer, a download answer, a quota-exhausted answer |

Implementation notes (`opensubtitles.rs`, trait in `subtitle_provider.rs`):

- A search sends `tmdb_id`, else `imdb_id`, else `query` and `year`, plus
  `moviehash`, `languages` and `type=movie`. The kinds wanted become
  `hearing_impaired` and `foreign_parts_only` (`only` or `exclude`; the
  default `include` is not sent). Parameters are sorted by name, as the
  provider asks for its cache. Only the first page (50 results) is read. A
  query with no id, title or hash makes no call.
- A candidate's id is the `file_id` of its first file, which is what
  `POST /download` takes. A result with no file or no language is dropped.
  The kind is `forced` when `foreign_parts_only`, else `hi` when
  `hearing_impaired`, else `normal`.
- Search needs only the `Api-Key`. The login runs on the first download;
  the token is kept, and one HTTP 401 on a download makes the client log in
  again once. A missing username or password is `Unauthorized` without a call.
- The quota is kept from the `remaining` and `reset_time_utc` fields of each
  download answer. `quota()` returns it so the caller can store it
  (`subtitles.opensubtitles.quota`) and give it back with `with_quota()`.
  With none left, `download` returns `QuotaExhausted` without a request until
  the reset time. HTTP 406 is the provider's "download count exceeded". A 429
  on a download that the retries (three, honoring `Retry-After`) did not
  clear is treated the same way. When an answer gives no reset time, the
  client assumes one hour.
- The answer to `POST /download` holds a `link` to the file. The client
  fetches it through the outbound check, without the key or the token.
- The `base_url` that `/login` returns (for VIP accounts) is ignored; the
  configured base URL is used.
- The fixtures in `fixtures/opensubtitles/` are written by hand from the
  public API documentation as known when this was written. The documentation
  site could not be read and no real account was used. Check the field names
  against a real answer before the first release.

### Which subtitle is chosen

A candidate is rejected when its kind is not the one the profile item asks
for, or when it is machine or AI translated (setting
`subtitles.exclude_machine_translated`, default on). The others get a score
out of 100:

| Match | Points |
|---|---|
| Same TMDB id (always true for a search by id) | 40 |
| Hash of the video file matches | 30 |
| Release group in the file name equals the candidate's | 12 |
| Source (`Bluray`, `WEBDL` and so on) equals | 8 |
| Resolution equals | 5 |
| Uploader is trusted by the provider | 5 |

The best candidate at or above `subtitles.minimum_score` (default 60) is
downloaded. A tie goes to the higher download count. Below the minimum the
subtitle stays wanted and the manual search in the UI lists every
candidate with its score. A hash match plus the TMDB id is enough (70); the
id plus the release group and the source is enough (60); the id alone is not.

### Download flow

1. For a wanted item, search with the movie's ids and the hash of the file.
2. Score the candidates and take the best one at or above the minimum.
3. `POST /download`, write the file next to the video with the name in
   [09-filesystem.md](09-filesystem.md), through a temporary file and a
   rename, and add the `subtitles` row.
4. Run after an import, and on the schedule
   `tasks.subtitle_search_interval_hours` (default 24) for items still wanted.
   A movie is searched at most once per run per item.

## Radarr as import source (M6)

| | |
|---|---|
| Endpoints (v3, read-only) | `GET /api/v3/system/status`, `/movie`, `/collection`, `/rootfolder`, `/qualityprofile`, `/qualitydefinition`, `/customformat`, `/tag`, `/indexer`, `/downloadclient` |
| Auth | `X-Api-Key` header |
| Rate limits | None |
| Failure | Import stops with a report; nothing partial is hidden; safe to re-run |
| Mock | Recorded responses from a real Radarr, shared with the contract tests |

Radarr masks secret fields in `/indexer` and `/downloadclient` responses.
Open question: import those entries disabled and ask the user to re-enter the
secret.

## Sonarr (M8)

v3, same pattern with `/series`. Not built yet.

## Prowlarr as import source (M11)

| | |
|---|---|
| Endpoints (v1, read-only) | `GET /api/v1/indexer` |
| Auth | `X-Api-Key` header |
| Rate limits | None |
| Failure | Import stops with a report; safe to re-run |
| Mock | Fixture `fixtures/prowlarr/indexer.json`, shaped after the v1 answer |

Every Prowlarr indexer becomes one indexer here: a torrent indexer
becomes Torznab, a usenet indexer becomes Newznab. Both point at
Prowlarr's per-indexer feed, `<prowlarr>/<indexerId>/api`, and carry the
import source's Prowlarr API key as their key. Imported indexers arrive
enabled.

The original definition is not rebuilt: Prowlarr masks each indexer's own
key, and a Cardigann definition signs in with a user name and password
that Unseenarr does not model. The feed also keeps Prowlarr's category
mapping. Prowlarr tags are not imported; they are Prowlarr ids and need
endpoints outside this issue.

## Decisions (2026-11-15)

- Optional proxy for indexer traffic: yes, in V1.
- Bazarr settings import: not in V1. Decided in the subtitles design: it is
  not practical enough to build. A Bazarr language profile has a few items
  that a user recreates in a minute, its provider logins are secrets that
  cannot be read back, and the part that matters, the subtitle files, is
  already adopted from disk because Bazarr and Unseenarr both name them
  `<video>.<language>[.hi|.forced].srt`. Bazarr's own API was not checked.
- Subtitle providers: OpenSubtitles only in V1, behind a trait.
