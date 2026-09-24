# Changelog

Notable changes in each release. Moekura follows [semantic
versioning](https://semver.org/); before 1.0, a minor release (0.2) may
change configuration or behaviour, and says so here. See
[Upgrading](docs/src/upgrading.md) for how to move between versions.

## [0.2.0] - 2026-09-24

The project is now called Moekura, from *moe* (萌え) and *kura* (蔵, a
storehouse). It lives at <https://github.com/moe-studios/moekura>.

### Changed

- **Breaking:** the program is now `moekura`, its config file
  `moekura.toml` and its environment variables `MOEKURA_*` (was
  `UWU_*`). `UWU_*` variables are ignored, so rename them before
  upgrading.
- **Breaking:** the container image is now `ghcr.io/moe-studios/moekura`,
  running as user `moekura` from `/var/lib/moekura`, with files in
  `/var/lib/moekura/data`.
- **Breaking:** the Docker Compose project is now `moekura`, with the
  Postgres role and database `moekura`. See below to keep your data.
- New API keys start with `mka_`. Existing `uwu_` keys keep working.
- Cookies are renamed, so everyone is logged out once.
- The Valkey `cache.prefix` default is now `moekura`. If you use Valkey
  and never set a prefix, the cache starts empty once.

### Upgrading

With a binary, rename the program, the config file and any `UWU_*`
variables (in systemd units too). The database and file paths are
whatever your config says, so they can stay.

With `deploy/compose.tiny.yml`, the new project name means new, empty
volumes. Move your data into them before starting 0.2.0:

```sh
# With 0.1.0 still checked out: stop it.
docker compose -f deploy/compose.tiny.yml down
git pull

# Copy the volumes to their new names.
for v in db files; do
  docker volume create moekura_$v
  docker run --rm -v uwubooru_$v:/from -v moekura_$v:/to alpine cp -a /from/. /to/
done

# Rename the Postgres role and database from uwu to moekura.
docker compose -f deploy/compose.tiny.yml up -d db
docker compose -f deploy/compose.tiny.yml exec db psql -U uwu -d postgres \
  -c 'CREATE ROLE rename_tmp SUPERUSER LOGIN'
docker compose -f deploy/compose.tiny.yml exec db psql -U rename_tmp -d postgres \
  -c 'ALTER ROLE uwu RENAME TO moekura' -c 'ALTER DATABASE uwu RENAME TO moekura'
docker compose -f deploy/compose.tiny.yml exec db psql -U moekura -d postgres \
  -c 'DROP ROLE rename_tmp'

docker compose -f deploy/compose.tiny.yml up -d
```

Once the site works, remove the old volumes with
`docker volume rm uwubooru_db uwubooru_files`.

## [0.1.0] - 2026-09-24

The first release.

### Posts and tags

- Upload images (JPEG, PNG, GIF, WebP, AVIF, optionally JPEG XL) and
  videos (MP4, WebM) from a file or a link. Exact duplicates are refused,
  and similar images are found by perceptual hash.
- Thumbnails, resized samples and video posters are made in the
  background.
- Tags with categories, aliases and implications (requested, approved and
  applied to existing posts), and autocomplete.
- Search with tags, `-exclusions`, `~either` groups, wildcards and
  metatags (rating, score, favorites, size, ratio, file type, date, user,
  parent, similar and more), with other orders and cursor paging.
- Post pages with editing, a full history with reverts,
  parent/child families, favorites, votes, blacklists and keyboard
  navigation.

### People and moderation

- Accounts with configurable roles and permissions. Registration can be
  open, by invite, approved by staff, or closed.
- An approval queue, flags, deletion with reasons, restore and purge;
  user and network bans; and a log of every staff action.
- Admin pages for site settings, users, roles, the job queue and storage.
- Private sites, where only logged-in users see anything and file links
  expire.

### API and tools

- A JSON API under `/api/v1` covering the site, with API keys, an
  OpenAPI 3.1 description, and a reference at `/api/docs`.
- `uwubooru admin import` for folders of files with gallery-dl, Hydrus or
  Danbooru sidecar files.
- `uwubooru admin seed` and `admin bench` for load testing.

### Running it

- One binary: `serve`, `worker`, `migrate` and admin commands, with
  settings from a file or `UWU_*` environment variables.
- Files on local disk or any S3-compatible store, optionally behind a CDN.
- PostgreSQL read replicas, with health and lag checks and
  read-your-writes.
- A shared Valkey for rate limits and cached counts across web servers.
- Searches stay under 50 ms (p95) with 5,000,000 posts.
- A 170 MB container image for amd64 and arm64.
