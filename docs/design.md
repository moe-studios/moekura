# uwuubooru — Design

uwuubooru is an open-source, self-hostable booru (a tag-based image board). The same codebase has to work for a single-user private instance on a small VPS or Raspberry Pi and for a public site with millions of posts, many web nodes and a CDN. This document records the stack, architecture, features, configuration and roadmap to the first release. Update it when a decision changes.

**Decisions made:** Rust backend · server-rendered HTML with light JS · PostgreSQL only · AGPL-3.0.

**Scaling rule:** one binary and one schema at every size. Growing an instance means changing configuration and adding processes. The code does not branch by instance size.

---

## 1. Tech stack

| Concern | Choice | Why |
|---|---|---|
| Language / runtime | Rust (stable, 2024 edition), `tokio` | One static binary, low memory use, fast media handling |
| HTTP | `axum` + `tower-http` (compression, timeouts, CORS, request IDs, tracing) | Mature and composable |
| Database | PostgreSQL 16+ via `sqlx` (compile-time-checked queries, built-in migrations) | Needed for GIN tag arrays, SKIP LOCKED queues and replicas |
| Queries | Static SQL strings with `sqlx::query_as` + `FromRow`; sqlx's `QueryBuilder` for the dynamic search SQL | Every query is covered by a `#[sqlx::test]` against real Postgres, so builds need no database or `.sqlx` cache |
| Templates | `minijinja` | Loaded at runtime, so admins can override templates and themes without recompiling |
| Frontend JS | TypeScript bundled with `esbuild` (the bundle is committed, so the Rust build needs no Node); `htmx` only if pages come to need it | Autocomplete, keyboard nav, note overlays, upload UI. Everything works without JS |
| CSS | Plain modern CSS with custom properties (no framework) | Easy to theme; light and dark by default |
| Asset embedding | `rust-embed` (static assets, default templates, migrations) | Keeps the single-binary deploy |
| Image processing | `libvips` via FFI (`libvips` crate) | Fast, low memory; handles JPEG/PNG/GIF/WebP/AVIF/JXL/APNG |
| Video | `ffmpeg` / `ffprobe` run as subprocesses | Probing, poster frames and preview clips for MP4 and WebM |
| Hashing | `sha2` (canonical), `md5` (compatibility lookup), `image_hasher` (perceptual hash) | Deduplication and similar-image search |
| File storage | `object_store` crate | One interface for local disk, S3-compatible stores (MinIO, Garage, R2, B2, SeaweedFS), GCS and Azure |
| Cache | `moka` in-process by default, optional Valkey/Redis via `fred` | Small instances need no Redis |
| Auth | `argon2` (argon2id), cookie sessions stored in PG, API keys, optional OIDC (`openidconnect`) | SSO for private instances |
| Rate limiting | `governor` in memory, Valkey-backed when running several nodes | |
| Config | `figment` (TOML file, then `UWUU_*` env vars, then CLI flags) | 12-factor and docker-friendly |
| CLI | `clap` | Subcommands for each process role |
| Markup (wiki, comments) | `pulldown-cmark` + booru extensions (`post #123`, `[[wiki link]]`, spoilers), sanitized with `ammonia` | Safe user markup |
| i18n | `fluent-templates` | Community translations |
| API docs | `utoipa`, which generates OpenAPI 3.1 | Third-party clients |
| Observability | `tracing`, OpenTelemetry OTLP export (optional), Prometheus `/metrics` | |
| Errors / validation | `thiserror`, `anyhow` (binary only), `garde` | |
| Testing | `#[sqlx::test]` (a throwaway real-PG database per test), `insta` snapshots, Playwright e2e | No Docker socket needed; CI uses a Postgres service container |

---

## 2. Architecture

### Process roles (one binary, `uwuubooru <cmd>`)
- `serve`: HTTP (HTML + JSON API). Stateless, so it scales horizontally. Also runs job workers when `jobs.run_in_serve` is on (the default), so one process is enough for small instances.
- `worker`: background job runner. Can run as many worker processes as needed; set `jobs.run_in_serve = false` on web nodes when you do.
- `migrate`, `admin` (create user, reindex, recount tags, regenerate thumbnails, import, export), `check-config`.

### Deployment tiers (configuration only)
| Tier | Topology |
|---|---|
| Tiny/private | `uwuubooru serve` (with built-in workers) + Postgres container, files on local disk, in-process cache and queue workers |
| Medium | Reverse proxy (Caddy/nginx) serves `/data/*` directly (the app can emit `X-Accel-Redirect`), plus a separate `worker` process |
| Large/public | N `serve` nodes behind a load balancer, a dedicated worker pool, a PG primary with read replicas (`database.replicas`) and PgBouncer, Valkey, S3 storage with a CDN in front, and an optional search accelerator (see §4) |

### Workspace layout
```
Cargo.toml                    # workspace
crates/
  core/        # domain types, search-query parser + AST, permission model, markup. Pure, no IO
  db/          # sqlx repositories, search SQL builder, migrations/
  media/       # probe, thumbnail, transcode, hashing (libvips/ffmpeg)
  storage/     # object_store wrapper, path layout, signed URLs
  jobs/        # PG job queue + job handlers
  web/         # axum routers: html/, api/v1/, compat/danbooru/, auth, middleware
  app/         # binary: clap CLI, config loading, wiring
frontend/      # TypeScript + esbuild → crates/web/static/js (committed; CI checks it is current)
crates/web/templates/  # default minijinja templates (overridable via paths.templates_override)
crates/web/static/     # CSS, icons; served under content-hashed URLs (overridable via paths.static_override)
locales/       # fluent .ftl files
deploy/        # compose (tiny + scaled), systemd unit, Caddy/nginx examples; Helm later (Dockerfile at repo root)
docs/          # mdBook: admin guide, API, search syntax, scaling guide
```

### Request flow
1. Middleware runs in order: request ID → tracing → rate limit → session/API-key auth → CSRF (HTML forms) → handler.
2. Handlers call service functions in `core`/`db`. HTML and API handlers share the same services, so there is no logic duplication.
3. Writes go to the PG primary. Reads marked replica-safe (listings, search, tag pages) go to replicas when replicas are configured.
4. Mutations enqueue jobs in the same transaction as the write, so jobs are never lost (outbox pattern).

### Job queue (Postgres-backed, no extra dependency)
- A `jobs` table with `kind, payload jsonb, run_at, attempts, locked_by, locked_until`. Workers claim with `FOR UPDATE SKIP LOCKED`, and `LISTEN/NOTIFY` wakes idle workers.
- Retry uses exponential backoff, with a dead-letter state visible in the admin UI.
- Job kinds: `process_upload`, `generate_variants`, `compute_phash`, `rewrite_tags` (alias/implication application), `recount_tags`, `purge_post`, `import_batch`, `prune_sessions`, `rebuild_search_index`.

---

## 3. Data model (core tables)

- **users**: id, name (unique, case-insensitive), email, password_hash, role_id, level, created_at, settings jsonb (blacklist, theme, per-page count), upload_limit fields.
- **roles** / **permissions**: configurable roles (default: Anonymous, Member, Contributor, Janitor, Moderator, Admin), each with permission flags. Instances can add or rename roles.
- **posts**: `id bigserial` (users see sequential IDs), uploader_id, `rating` (g/s/q/e), `status` (pending/active/flagged/deleted), source, description, parent_id, score, fav_count, `tag_ids int4[]` (**GIN index with `intarray` `gin__int_ops`**), cached `tag_count_*` per category, created_at, updated_at.
- **media_assets**: post_id, sha256 (unique), md5 (indexed), mime, width, height, duration, file_size, `phash bigint` (plus four `int2` chunk columns for the multi-index hamming search), storage key.
- **media_variants**: asset_id, kind (thumb_sm/thumb_lg/sample/preview_video), format (webp/avif), dims, storage key.
- **tags**: id, name (unique), category_id, post_count, is_deprecated, created_at. `post_count` counts active and flagged posts and is kept exact by statement-level triggers on `posts`. **tag_categories** are configurable; the defaults are general, artist, character, copyright and meta, with Danbooru's ids.
- **tag_relations**: aliases (antecedent → consequent) and implications in one table, by tag name, with status pending/active/rejected/deleted. Both go through a request/approval workflow; approval queues a job that rewrites existing posts.
- **wiki_pages** (+ versions), **pools** (ordered `post_ids bigint[]`, category series/collection, + versions), **notes** (x, y, w, h, body, + versions), **comments** (+ votes), **favorites**, **post_votes**, **saved_searches**.
- **post_versions**: diff history of tags, rating, source and parent, used for undo and history pages.
- **flags**, **appeals**, **reports**, **bans**, **ip_bans**, **mod_actions** (audit log), **sessions**, **api_keys**, **invites**, **site_settings** (key/value jsonb), **jobs**.

Files are stored under content-addressed keys: `original/ab/cd/<sha256>.<ext>`, `thumb/<size>/ab/cd/<sha256>.webp`.

---

## 4. Tag search (the main scaling risk)

**Syntax** is Danbooru-style, so it is familiar to users (full reference in [search.md](search.md)):
`tag1 tag2 -excluded ~or_a ~or_b wild*card rating:e,q score:>=10 favcount:>5 user:name parent:123 width:>1920 ratio:16:9 date:2026-01..2026-06 md5:… filetype:png,webm status:deleted order:score|favcount|random|id_asc|… limit:40`. `fav:`, `pool:` and `similar:` come with the features they search.

**Pipeline:**
1. `uwuu_core::search::Query::parse` turns the query into an AST. Errors come back as structured values that the UI shows inline.
2. `uwuu_db::search::Plan` resolves aliases and expands wildcards (capped, highest-count tags first).
3. The planner estimates matches from exact tag counts (assuming independence) and picks a strategy:
   - AND/NOT/OR tags compile to `tag_ids @> '{…}'`, `NOT tag_ids && '{…}'` and `tag_ids && '{…}'`, all backed by GIN.
   - When many posts are expected to match, it walks the order's btree (`id`, `score`, `fav_count`) with the tag condition written as `(…) IS TRUE` so Postgres can't use GIN. When few are, it collects matches through GIN and sorts, with the order column written as `col + 0` so Postgres can't walk. Walking reads about (offset + limit) × total ÷ matches rows; collecting reads every match.
   - Metatags compile to column predicates.
4. **Pagination:** numbered pages up to a configurable depth (default 1000), keyset `page=b<id>` / `page=a<id>` beyond that (id order only).
5. **Counts:** exact up to `search.count_limit` (default 10,000); above that, a single tag reads `tags.post_count`, an empty search reads the table estimate, and anything else shows "10,000+". Caching counts comes with the cache layer.
6. Search sits behind a `SearchBackend` trait. The default Postgres backend should handle a few million posts. An optional external engine can be plugged in later for very large sites without changing callers.

**Similar-image search:** a 64-bit perceptual hash split into four 16-bit chunks, each indexed. By the pigeonhole principle, any image within hamming distance ≤3 matches at least one chunk exactly, so indexed lookups narrow candidates and exact distance is checked in SQL. Similar images are shown as a warning on upload and exposed as a `similar:<post_id>` search.

---

## 5. Features

### MVP (v0.1)
- Upload from a file or URL (drag-drop, multiple files) with rating, tags, source and parent. Exact duplicates are rejected by sha256, and similar images trigger a warning.
- Media: JPEG, PNG, GIF, WebP, AVIF and JXL images; animated GIF/APNG/WebP; MP4 and WebM. Thumbnails in two sizes plus a "sample" (resized large) variant. Output format is configurable (WebP by default, AVIF optional).
- Post page: image or video view, tag sidebar grouped by category, source, rating, score/favorite, parent/child bar, history, keyboard navigation between search results.
- Tag search as in §4, with autocomplete (prefix search plus aliases, ranked by count).
- Tags: categories, aliases, implications (applied by jobs), wiki pages, tag list/search, and tag edit history.
- Users: registration modes (open / invite-only / closed / admin-approval), login, profile, favorites, per-user blacklist (filtered server-side for rendering, and client-side as a fallback), per-page and theme settings.
- Moderation: roles and permissions, a post approval queue (can be turned off), flag/report → review, soft delete with reason → trash → purge, bans, IP bans and a mod-action audit log.
- Admin panel: site settings, roles, tag categories, job queue status, storage stats.
- Private mode: login required to view anything, with signed and expiring file URLs.
- JSON REST API `/api/v1` covering the web UI features, with API keys, OpenAPI spec and docs page.
- CLI bulk import: a folder with sidecar `.txt`/`.json` tag files.

### v0.2+
- Pools (ordered, for comics and series) with a reader view, notes (translation overlays) with an editor, comments with votes, saved searches, and favorite groups.
- **Danbooru-compatible API layer** (`/posts.json`, `/tags.json`, …) so existing clients such as Grabber, Boorusama and gallery-dl work unchanged.
- Upload limits and a contribution-level system, tag change requests with voting, bulk tag editing (mass tag edit / tag scripts).
- Atom/RSS feeds for searches, webhooks, and import from other boorus via their public APIs.
- Optional ML auto-tagger: a separate process running a WD-tagger ONNX model (`ort` crate) that consumes `process_upload` events and posts tag suggestions.

### Out of scope (maybe later)
Federation (ActivityPub), a native mobile app, a plugin runtime (WASM).

---

## 6. Configuration

There are two layers, kept deliberately separate:

**Infrastructure config** (`uwuubooru.toml`, which env vars `UWUU_SECTION__KEY` override). Changing it requires a restart:
```toml
[server]      bind = "0.0.0.0:8080", public_url, trusted_proxies, serve_files = true|false|"x-accel"
[database]    url, replicas = [], max_connections, statement_timeout
[storage]     backend = "local"|"s3", path / bucket, endpoint, region, public_base_url (CDN), signed_urls
[cache]       backend = "memory"|"valkey", url
[jobs]        workers = 4, run_in_serve = true
[media]       thumb_sizes, sample_size, output_format = "webp"|"avif", max_upload_mb, ffmpeg_path, allowed_types
[auth]        session_ttl, oidc { issuer, client_id, client_secret, button_label }
[mail]        smtp settings (optional; required for email verification/reset)
[telemetry]   log_format = "pretty"|"json", otlp_endpoint, metrics = true
[paths]       templates_override, static_override, locales_override
```

**Site settings** (stored in the DB and edited from the admin panel, no restart): site name, description, logo, registration mode, default rating filter for anonymous users, whether uploads need approval, upload limits per role, tag categories, blacklist defaults, pagination limits, content rules page and footer links.

`uwuubooru check-config` validates the config and prints the effective settings with secrets redacted.

---

## 7. Security

- CSRF tokens on HTML forms, `SameSite=Lax` cookies, a strict CSP (no inline JS) and `nosniff`. User-uploaded files are served with `Content-Disposition` and, ideally, from a separate origin or CDN domain.
- Uploads are validated by content sniffing, not by extension. Decoding is sandboxed by limits (pixel count cap and decompression-bomb guard), ffmpeg runs with timeouts and resource limits, SVG is disallowed by default, and image metadata (EXIF/GPS) is stripped from generated variants (optionally from originals too).
- URL uploads are fetched with SSRF protection: private IP ranges are blocked and there are size and time caps.
- argon2id passwords, rate-limited login, optional TOTP 2FA (v0.2) and hashed API keys.

---

## 8. Tooling & project hygiene

- CI (GitHub Actions): `cargo fmt --check`, `clippy -D warnings`, `nextest` against a PG service container, `sqlx prepare --check`, frontend typecheck and build, `cargo deny` (licenses and advisories), and Playwright e2e against a compose stack.
- Releases: multi-arch (amd64/arm64) Docker images published to GHCR, static-ish binaries (glibc; libvips/ffmpeg as runtime deps in the image), and a changelog from conventional commits (`git-cliff`).
- Docs: an mdBook covering installation (compose and bare metal), configuration reference, search syntax, API, scaling guide and backups (`pg_dump` plus a storage sync).
- Repo files: `LICENSE` (AGPL-3.0), `README`, `CONTRIBUTING`, `CODE_OF_CONDUCT`, `SECURITY.md`, issue templates.

---

## 9. Roadmap to v0.1

1. **Skeleton:** workspace, config loading, `serve`/`migrate` commands, health endpoint, CI, Dockerfile and tiny-tier compose.
2. **Schema and auth:** migrations for users, roles, sessions and posts; register/login; permission middleware.
3. **Media pipeline:** storage abstraction, upload endpoint, PG job queue, probe/thumbnail/hash jobs, file serving.
4. **Tags and search:** tags, aliases, implications, query parser (heavily unit-tested), SQL planner, listing pages, autocomplete.
5. **Post UI:** post page, editing, history, favorites and votes, blacklist, keyboard navigation.
6. **Moderation and admin:** approval queue, flags, deletions, bans, audit log, site settings UI, private mode.
7. **API and docs:** `/api/v1` parity, OpenAPI, mdBook docs, bulk import CLI.
8. **Scale hardening:** a seeding tool that generates 5M+ synthetic posts with a realistic (Zipf) tag distribution, query plan checks, read-replica routing, Valkey backend, S3 + CDN path.

---

## Verification

- **Unit:** search parser and planner (table-driven plus `insta` snapshots of the generated SQL), permission checks, markup sanitization.
- **Integration:** `#[sqlx::test]` against real Postgres, covering upload → job → variants → searchable, alias and implication rewrites, tag counts staying consistent after edits and deletes.
- **E2E:** Playwright against `docker compose -f deploy/compose.tiny.yml up` for register, upload, tag, search, favorite, moderate, and private-mode access denial.
- **Scale:** `uwuubooru admin seed --posts 5000000` followed by a benchmark suite (`oha`/`k6`) over common, rare, negated, OR and deep-page searches. Target p95 below 100 ms for first-page searches on commodity hardware, and a regression check in CI on a smaller seed.
- **Tiny-tier check:** the `all` process stays under about 150 MB RSS when idle, and the full compose stack runs on a 1 GB VPS or a Raspberry Pi 4.
