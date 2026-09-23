# uwuubooru

A self-hostable booru (tag-based image board) that scales from a private
single-user instance to a public site with millions of posts, using the same
binary and schema at every size.

> **Status: early development.** Accounts, uploads, thumbnails, tags,
> aliases, implications and search work; the post UI (editing, favourites)
> comes next. See [docs/design.md](docs/design.md) for the plan and roadmap,
> and [docs/search.md](docs/search.md) for the search syntax.

## Running

The smallest setup is the app plus Postgres, via Docker or Podman compose:

```sh
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > deploy/.env
docker compose -f deploy/compose.tiny.yml up -d
curl localhost:8080/readyz   # → ok
```

The container image includes the media tools, which make up most of its
size (about 640 MB).

Without containers, you need PostgreSQL 16 or newer, a database the app
owns, and the media tools:

| Tool | Used for | Fedora | Debian / Ubuntu |
|---|---|---|---|
| libvips 8.15+ (`vips`, `vipsheader`, `vipsthumbnail`) | reading images, thumbnails, perceptual hashes | `vips-tools` (AVIF: `vips-heif`, JPEG XL: `vips-jxl`) | `libvips-tools libheif-plugin-dav1d libheif-plugin-aomenc` |
| ffmpeg (`ffmpeg`, `ffprobe`) | reading videos, poster frames | `ffmpeg` (RPM Fusion) or `ffmpeg-free` | `ffmpeg` |

`serve` and `worker` check for them at startup.

```sh
cargo build --release
UWUU_DATABASE__URL=postgres://uwuu:secret@localhost/uwuu target/release/uwuubooru serve
```

### Files and scaling

Uploads and thumbnails go to `storage.path` (`./data` by default), or to
any S3-compatible bucket with `storage.backend = "s3"`. Back files up
together with the database. Set `storage.public_base_url` when a CDN or
public bucket serves the files; otherwise the app serves them at `/data/`.

`serve` runs background jobs (thumbnails, hashing) itself. For busier sites,
run one or more `uwuubooru worker` processes and set
`jobs.run_in_serve = false` on the web nodes.

### Commands

| Command | Purpose |
|---|---|
| `uwuubooru serve` | Run the HTTP server, plus background job workers unless `jobs.run_in_serve = false` (applies migrations first unless `database.auto_migrate = false`) |
| `uwuubooru worker` | Run background job workers only, for scaling them separately from the web nodes |
| `uwuubooru migrate` | Apply pending migrations and exit, for release pipelines |
| `uwuubooru check-config` | Validate configuration and print it with secrets redacted |
| `uwuubooru admin …` | Create users, change roles, view and change site settings |

### First admin account

Create it from the shell (it prompts for a password, or reads one line from
stdin when piped):

```sh
uwuubooru admin create-user yourname --role admin
# with compose:
docker compose -f deploy/compose.tiny.yml exec app uwuubooru admin create-user yourname --role admin
```

Registration is open by default. To change that before the admin panel
exists:

```sh
uwuubooru admin settings                               # show all settings
uwuubooru admin settings set registration_mode closed  # open | invite | approval | closed
```

`GET /healthz` reports that the process is up. `GET /readyz` also checks
the database; point load balancers at it.

## Configuration

Settings come from built-in defaults, then `uwuubooru.toml` (or
`--config <path>` / `UWUU_CONFIG`), then environment variables, each layer
overriding the last. Every key maps to `UWUU_<SECTION>__<KEY>`, e.g.
`UWUU_DATABASE__URL`. Unknown keys are rejected, so typos fail loudly.

See [`uwuubooru.example.toml`](uwuubooru.example.toml) for every option.

## Development

Requires Rust 1.94+, the media tools above, and a Postgres server for the
database tests:

```sh
podman run -d --name uwuu-pg -p 55432:5432 \
  -e POSTGRES_USER=uwuu -e POSTGRES_PASSWORD=uwuu -e POSTGRES_DB=uwuu \
  docker.io/library/postgres:18-alpine

export DATABASE_URL=postgres://uwuu:uwuu@localhost:55432/uwuu
cargo test --workspace          # each DB test gets its own throwaway database
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

The page scripts are TypeScript in `frontend/`, bundled with esbuild into
`crates/web/static/js/`. The bundle is committed, so building the binary
needs no Node; after changing `frontend/src`, rebuild it (Node 24+):

```sh
cd frontend
npm ci
npm run check     # typecheck
npm run build     # or `npm run watch`
```

Layout:

```
crates/core     domain types and pure logic (config, permissions, accounts, settings)
crates/db       Postgres pools, read-replica routing, migrations, queries
crates/storage  file storage: local disk or S3
crates/media    identifying and processing media with vips and ffmpeg
crates/jobs     the Postgres job queue's workers and job handlers
crates/web      axum router, middleware, pages
crates/app      the `uwuubooru` binary: CLI, config loading, logging
frontend/       TypeScript for the pages (built into crates/web/static/js)
deploy/         compose files and deployment examples
```

## License

[AGPL-3.0-only](LICENSE). If you run a modified version as a public
service, you must publish your changes.
