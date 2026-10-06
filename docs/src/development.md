# Development

Moekura is written in Rust, with pages rendered on the server and a little
TypeScript on top.

You need Rust 1.94+, the [media tools](install/bare-metal.md), and a
PostgreSQL server for the database tests:

```sh
podman run -d --name moekura-pg -p 55432:5432 \
  -e POSTGRES_USER=moekura -e POSTGRES_PASSWORD=moekura -e POSTGRES_DB=moekura \
  docker.io/library/postgres:18-alpine

export DATABASE_URL=postgres://moekura:moekura@localhost:55432/moekura
cargo test --workspace          # each database test gets its own database
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check                # licenses, advisories, sources
```

The page scripts are TypeScript in `frontend/`, bundled with esbuild into
`crates/web/static/js/`. The bundle is committed, so building the binary
needs no Node; after changing `frontend/src`, rebuild it (Node 24+):

```sh
cd frontend
npm ci
npm run check     # typecheck
npm test          # unit tests
npm run build     # or npm run watch
```

## Build version

The footer embeds the version when the Rust binary is compiled. A checkout
at the release tag matching `Cargo.toml` shows that semver; other commits
show `git-` followed by the first seven characters of the commit hash.
`MOEKURA_BUILD_VERSION` overrides detection, so CI can distinguish release
builds from main builds even when they share a commit.

For a local Docker build, pass the version explicitly because `.git` is
excluded from the build context:

```sh
docker build --build-arg MOEKURA_BUILD_VERSION="git-$(git rev-parse --short=7 HEAD)" -t moekura .
```

For a release build, pass its semver instead. Source archives without Git
metadata also need `MOEKURA_BUILD_VERSION`; without it, the footer displays
`git-unknown` and the build emits a warning.

## End-to-end tests

`e2e/` has [Playwright](https://playwright.dev/) tests that drive a real
browser through a running site: registering, uploading, tagging,
searching, the wiki, favorites, moderation and private mode. CI runs them
against `deploy/compose.tiny.yml`; to run them yourself, see
[`e2e/README.md`](https://github.com/moe-studios/moekura/blob/main/e2e/README.md).

## Renaming something public

What [Stability](stability.md) promises can't be renamed outright within a
major version: the old name keeps working, with a warning, until the next
one. Each kind has its shim:

- **Config keys**: rename the field, then add the old and new dotted paths
  to `CONFIG_KEYS` in `crates/app/src/deprecated.rs`. The old key, in the
  file or as its `MOEKURA_*` variable, is moved to the new one before the
  configuration is read, and startup logs a warning naming the new key.
  Setting both is an error.
- **CLI flags and subcommands**: rename it, then add it to `CLI_NAMES` in
  the same file (`"admin seed --batch"` → `"--batch-size"`). Old spellings
  are rewritten before parsing, so they stay out of `--help`, and print a
  warning.
- **API operations**: add the method and path to `DEPRECATED` in
  `crates/web/src/api/mod.rs`. The OpenAPI description marks it
  `deprecated` and says what to use instead, and its responses carry a
  `Deprecation` header ([RFC 9745](https://www.rfc-editor.org/rfc/rfc9745)).
- **API fields**: keep sending the old field beside the new one, and mark
  it with `#[schema(deprecated)]`.

Tests check that each old config key and CLI name leads to one that exists.
Say what was renamed in the changelog's upgrading notes, and remove the
shims in the next major release.

## This book

The documentation is an [mdBook](https://rust-lang.github.io/mdBook/) in
`docs/`:

```sh
mdbook serve docs    # http://localhost:3000, rebuilt as you edit
```

CI builds it and checks its links on every pull request. Pushes to `main`
publish it to GitHub Pages once the repository is public (set **Settings →
Pages → Source** to *GitHub Actions*).

## Releasing

1. Draft the changelog entry from the commits since the last release, then
   edit it into notes people can read:
   `git cliff --unreleased --tag vX.Y.Z --prepend CHANGELOG.md`.
2. Set the workspace version in `Cargo.toml`, and the date in the
   changelog heading.
3. Merge that, then tag the merge commit `vX.Y.Z` and push the tag. The
   Release workflow checks the version and changelog, publishes the image
   and creates the GitHub release with the binaries.

## Layout

```text
crates/core     domain types and pure logic (config, permissions, search syntax)
crates/db       PostgreSQL pools, migrations, queries, the search planner
crates/storage  file storage: local disk or S3
crates/media    identifying and processing media with vips and ffmpeg
crates/jobs     the job queue's workers and handlers
crates/web      the HTTP server: pages, the API, middleware
crates/app      the moekura binary: commands, configuration, logging
frontend/       TypeScript for the pages
deploy/         compose files
docs/           this book
```
