# 12 Testing strategy

Status: reviewed (2026-11-16).

## Layers

| Layer | What | Where |
|---|---|---|
| Unit | Parsing, scoring, naming templates, permission checks, state machines. Table-driven. | `crates/core` |
| Database | Queries and migrations against a temp SQLite file per test. Migrations run from zero on every run. | `crates/db` |
| Integration | The Axum app in-process, real SQLite, external services mocked with `wiremock`. | `crates/api/tests` |
| Route permission coverage | Walks the router: every route declares one permission or is public, and matches `x-permission` in OpenAPI. | `crates/api/tests` |
| Permission behaviour | For each route: 401 without credentials, 403 without the permission, success with it. Generated from the OpenAPI document. | `crates/api/tests` |
| Compat contract | Our responses validate against the upstream Radarr OpenAPI schema; recorded request/response pairs from a real Radarr are the fixtures. | `crates/arr-compat/tests` |
| Frontend unit | Stores and helpers with Vitest. | `web` |
| End to end | A few Playwright tests against the real binary: setup, login, add movie, see queue. | `web/e2e` |

## Fixtures

- `fixtures/tmdb`, `fixtures/torznab`, `fixtures/qbittorrent`: recorded
  responses, secrets removed.
- `fixtures/radarr`: recorded pairs per implemented endpoint, with the Radarr
  version in the file name.
- `fixtures/prowlarr`: a v1 GET /api/v1/indexer answer, shaped from the API schema.
- The release-name table in [07-release-scoring.md](07-release-scoring.md) is
  a data file read by the tests and by the doc check.

## Compat contract tests

1. Record: a script calls a real Radarr and stores the pairs.
2. Schema check: each of our responses validates against the pinned Radarr
   OpenAPI document. No field in the upstream schema is omitted.
3. Shape check: for the same request, our response has the same set of keys
   and types as the recorded one.
4. Client check: one test replays the exact request sequence that Seerr sends.

This method is final (review of 2026-11-16). Steps 2 to 4 run in CI from M6;
step 1 is run by hand when the pinned Radarr version changes.

## CI

`.github/workflows/ci.yml` runs four jobs on every pull request and on every
push to `main`:

| Job | Steps |
|---|---|
| Rust | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `just coverage` (all Rust tests under `cargo llvm-cov nextest`, with the minimums below); `just gen`, then `git diff --exit-code` |
| Web | `bun run check` (static-SPA check and `svelte-check`), `bun run lint` (ESLint), `bun run test` (Vitest with coverage), `bun run build` |
| Core tests on Windows | `cargo nextest run -p core` |
| Docker image | Builds the image, no push |

The Rust job runs in an `ubuntu:26.04` container, because the auth library
links OpenSSL 3 and ICU 78 (ADR 0006).

Planned, not in the workflow yet:

- The importer tests on Windows, when the importer exists (M4,
  [09-filesystem.md](09-filesystem.md)).
- The compat schema check, with the contract tests (M6).
- On `main` only: the Playwright tests, and the image push on a release (M6).

## Coverage

Aim: as high as the code allows. Measured with `cargo llvm-cov` and Vitest
coverage on every pull request.

| Scope | Minimum line coverage |
|---|---|
| `crates/core` (parsing, scoring, custom formats, naming, permissions, state machines) | 90% |
| Rust workspace overall | 80% |
| `web/src/lib` (stores, helpers, API wrappers) | 80% |

Not counted, because no test of ours can or should cover it:

- Rust: `crates/db/src/auth_schema.rs` (generated) and `crates/api/src/main.rs`
  (process start; the end-to-end tests run it).
- Web: the vendored shadcn-svelte code (`components/ui`, `hooks`, `utils.ts`),
  the generated `api/schema.d.ts`, and modules that only create an instance
  (`api/client.ts`). The list is in `web/vite.config.ts`.

`just coverage` runs the Rust part locally; `bun run test` runs the web part.

A pull request fails below a minimum. The minimums go up when the real
numbers allow it; they never go down.

## Not tested automatically

- Real indexers, real TMDB.
- A real qBittorrent, except one optional local test.
- Seerr end to end: a manual checklist in M6. Not automated with containers in CI.

## Decisions (2026-11-15)

- Coverage: measured and enforced with the minimums above.
- The Seerr test stays manual. The replayed Seerr request sequence in the
  contract tests covers regressions in CI.
