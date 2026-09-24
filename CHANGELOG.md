# Changelog

Notable changes in each release. Moekura follows [semantic
versioning](https://semver.org/); before 1.0, a minor release (0.2) may
change configuration or behaviour, and says so here. See
[Upgrading](docs/src/upgrading.md) for how to move between versions.

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
