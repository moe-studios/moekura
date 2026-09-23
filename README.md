# uwuubooru

A self-hostable booru (tag-based image board) that scales from a private
single-user instance to a public site with millions of posts, using the same
binary and schema at every size.

> **Status: early development.** Only the server skeleton exists so far. See
> [docs/design.md](docs/design.md) for the plan and roadmap.

## Running

The smallest setup is the app plus Postgres, via Docker or Podman compose:

```sh
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > deploy/.env
docker compose -f deploy/compose.tiny.yml up -d
curl localhost:8080/readyz   # → ok
```

Without containers, you need PostgreSQL 16 or newer and a database the app
owns:

```sh
cargo build --release
UWUU_DATABASE__URL=postgres://uwuu:secret@localhost/uwuu target/release/uwuubooru serve
```

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

Requires Rust 1.94+ and a Postgres server for the database tests:

```sh
podman run -d --name uwuu-pg -p 55432:5432 \
  -e POSTGRES_USER=uwuu -e POSTGRES_PASSWORD=uwuu -e POSTGRES_DB=uwuu \
  docker.io/library/postgres:18-alpine

export DATABASE_URL=postgres://uwuu:uwuu@localhost:55432/uwuu
cargo test --workspace          # each DB test gets its own throwaway database
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Layout:

```
crates/core   domain types and pure logic (config, later: search parser, permissions)
crates/db     Postgres pools, read-replica routing, migrations
crates/web    axum router, middleware, HTTP handlers
crates/app    the `uwuubooru` binary: CLI, config loading, logging
deploy/       compose files and deployment examples
```

## License

[AGPL-3.0-only](LICENSE). If you run a modified version as a public
service, you must publish your changes.
