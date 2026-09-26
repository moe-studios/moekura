# Changelog

Notable changes in each release. Moekura follows [semantic
versioning](https://semver.org/); before 1.0, a minor release (0.2) may
change configuration or behaviour, and says so here. See
[Upgrading](docs/src/upgrading.md) for how to move between versions.

## [0.4.0] - 2026-09-26

Posts gain comments, notes and pools, contributors get tools for
larger edits, and a site can talk to the outside world through feeds,
webhooks and link previews. An optional tagger suggests tags with a
machine-learning model. Upgrading needs nothing beyond the usual
steps, but read the notes on roles and `ai:` tags below.

### Community

- Comments on posts, in the wiki markup with quotes and replies, with
  votes, reports and a moderation queue. `/comments` lists recent ones.
- Pools: ordered series and collections with an editor, history and
  reverts, pool navigation on post pages and a reader for series.
- Notes: translation boxes drawn over images, with an editor that works
  with a mouse, pen or touch, and a history with reverts.
- Saved searches, found again with `search:all` or `search:<label>`.
- Favorite groups, public or private.
- New search terms: `commentcount:`, `order:comment`, `notecount:`,
  `order:note`, `note:`, `pool:`, `ordpool:`, `favgroup:`,
  `ordfavgroup:` and `search:`.

### Contributing

- Upload limits per role, for uploads waiting in the approval queue and
  uploads a day, optionally scaling with a user's record as on
  Danbooru (`upload_limit_scaling`).
- Members can be promoted to contributors automatically once their
  record meets the `promotion_rules` (`auto_promotion`).
- Votes and discussion on alias and implication requests, and bulk
  update requests: scripts of alias, imply, update and category lines,
  approved as a whole.
- Mass tag edits over a whole search, run in the background, and tag
  scripts applied by clicking posts in search results.

### Integrations

- Atom feeds for any search (`/posts.atom?tags=…`) and for comments,
  with feed tokens so readers see what their user sees.
- Outgoing webhooks for new, approved, deleted and flagged posts, new
  comments and new users, signed with HMAC-SHA256 and retried. See
  [Webhooks](docs/src/admin/webhooks.md).
- Link previews (OpenGraph and Twitter cards) for posts, pools and wiki
  pages, and oEmbed for posts. Only general and sensitive posts show an
  image unless `preview_all_ratings` is on; private sites show none.
- `moekura admin import-remote` copies posts, with their tags, notes
  and pools, from Danbooru, e621, Gelbooru, Moebooru and other Moekura
  sites, and carries on where it stopped. See
  [Bulk import](docs/src/admin/import.md).

### Tagger

- `moekura tagger` suggests tags and a rating for new posts with a
  WD-tagger model, on the CPU, in a process of its own that can run on
  another machine. Suggestions show when editing a post, and those it is
  sure of can be applied automatically. `ai:tag` finds posts the tagger
  thinks have a tag they're missing, and `moekura admin tag-backlog`
  tags existing posts.
- The image is published a second time as `X.Y.Z-tagger`, with ONNX
  Runtime included, and `deploy/compose.tagger.yml` adds it to the tiny
  setup. See [Tagger](docs/src/admin/tagger.md).

### Danbooru apps

- Comments, pools, notes, favorite groups, saved searches and tagger
  suggestions (`/ai_tags.json`) answer with real data instead of empty
  lists, so apps show translations and pools.
- Uploads through the Danbooru API (`/uploads.json`, then
  `/posts.json`), as Boorusama does them.

### Fixed

- `moekura worker` exited a minute after starting, leaving any running
  job to be retried.

### Upgrading

- The new permissions (`moderate_comments`, `edit_pools`, `edit_notes`
  and `mass_edit_tags`) are given to the built-in roles that should have
  them. Roles you made yourself get none of them; add them on the Roles
  page.
- Members may now have at most 10 uploads waiting for approval at once.
  This only matters with the approval queue on; change it on the Roles
  page.
- `ai:` is now a search term, so tag names can't start with it. Tags
  that did are renamed to `ai_…` (or `ai_…_(tag)` if that's taken).

## [0.3.0] - 2026-09-25

Apps made for Danbooru now work with a Moekura site, and accounts gain
email, two-factor and single sign-on logins. Nothing needs changing to
upgrade; the new features are off until you configure them.

### Danbooru apps

- A Danbooru-compatible API, so gallery-dl, Grabber and Boorusama work
  unchanged: posts (with `b<id>`/`a<id>` paging and `only=`), counts,
  tags, autocomplete, aliases, implications and related tags, users and
  `/profile.json`, wiki pages, favorites, votes, post edits and post
  history. API keys work as HTTP Basic or `login` and `api_key`
  parameters. See [Danbooru apps](docs/src/using/danbooru-clients.md).
- Features Moekura doesn't have yet (pools, comments, notes and so on)
  answer with empty lists so apps carry on. Uploading through the
  Danbooru API isn't supported yet.

### Accounts

- Mail over SMTP, set up in a new `[mail]` section and checked with
  `moekura admin send-test-mail`.
- With mail set up, the `email_verification` site setting makes new
  accounts confirm their address, and anyone can reset a forgotten
  password by email.
- An account settings page for changing the email address and password.
- Two-factor login with an authenticator app (TOTP), with recovery codes.
  Staff who manage users can turn it off for someone who lost their
  device.
- Logging in through an OpenID Connect provider such as Authentik or
  Keycloak, set up in a new `[auth.oidc]` section. Existing users can
  link a provider account from their settings.

### Tags

- Wiki pages for tags at `/wiki/{tag}`, with history and reverts. A
  search for a single tag shows the start of its page, and the API gains
  `/wiki-pages` endpoints.

### Project

- Contributing, security and conduct guides, and issue forms.
- Dependencies are checked with `cargo deny`, and CI runs browser tests
  (Playwright) and gallery-dl against the built image.

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
