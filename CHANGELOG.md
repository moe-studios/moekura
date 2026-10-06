# Changelog

Notable changes in each release. Moekura follows [semantic
versioning](https://semver.org/): from 1.0 on, only a major release (2.0)
may break what [Stability](docs/src/stability.md) lists, and minor
releases list what they deprecate. Before 1.0, a minor release (0.2) could
change configuration or behaviour, and said so here. See
[Upgrading](docs/src/upgrading.md) for how to move between versions.

## [0.6.0] - 2026-10-06

Getting ready for 1.0: what 1.0 promises, how things get renamed after
it, CI that checks upgrades, backups and performance, and the fixes from
a security review. Upgrading is recommended. Read the upgrading notes
below: some defaults changed, and some API and webhook names.

### Stability

- [Stability](docs/src/stability.md) says what 1.0 promises not to break
  (the API, the Danbooru API as the tested apps use it, configuration,
  commands, search syntax, webhooks, feeds, metrics and image tags) and
  what isn't a contract (the database schema, overridden templates and
  translations, pages' HTML, logs).
- After a rename, old config keys (and their `MOEKURA_*` variables) and
  old command-line flags keep working, with a warning naming the new
  ones; deprecated API operations are marked in the OpenAPI description
  and answer with a `Deprecation` header.
- A support policy: which releases get security fixes after 1.0, and the
  oldest supported PostgreSQL, Valkey, libvips, ffmpeg, glibc and Rust
  (see [Upgrading](docs/src/upgrading.md#requirements)).
- From 1.0, releases are also tagged with their major version alone
  (`ghcr.io/moe-studios/moekura:1`).
- `/api/v1` users and `/me` have `role_key`: the built-in role's key
  (`member`, `moderator`, …), which doesn't change when an admin renames
  the role, or `null` for a role the site made.

### Operations

- CI upgrades a site seeded on the previous release to each new build
  and checks its data, backs it up and restores it as
  [Backups](docs/src/admin/backups.md) says (local storage and S3), and
  checks memory at idle and at peak and every page's speed with 200,000
  posts on the 1 GB stack. The backups page now covers restoring, and
  copying files with compose.
- X posts are read through an FxEmbed API (`sources.x.fxembed_api_url`),
  with a logged-in account as the fallback.

### Security

A review of the whole server before 1.0 fixed these, among many smaller
ones:

- Logins, sign-ups and mail are limited per network and per account,
  with a captcha past a looser ceiling; `X-Forwarded-For` is read from
  the right, and IPv4-mapped addresses as IPv4. Repeated wrong
  two-factor codes lock the account's codes for a while and warn its
  owner.
- Reset links end when the password or address changes, and a reset
  revokes API keys and feed tokens; new keys and tokens are confirmed.
  Only bare email addresses are taken and mailed to.
- OIDC logins finish only in the browser that started them, reach the
  provider only over https, and follow the sign-up rules and network
  bans.
- Uploads are rate limited per account, links start bounded downloads,
  and refused or abandoned uploads' files are removed. Zips, ugoira and
  the libvips tools are held to limits (`media.vips_memory_mb`,
  `media.vips_cpu_secs`), and ffmpeg gets only re-encoded frames.
- Source lookups, link previews, imports and webhooks are bounded and
  confined: a site's login is sent only to a work's own page, link
  previews see what a logged-out visitor sees, redirects carry no
  `Referer`, and webhooks send images only of posts visitors may see,
  escape Discord embeds and pause receivers that don't answer.
- Posts, pools, artists and users a viewer can't see stay out of pools,
  favorites, comments, `similar:`, feeds and the Danbooru API; personal
  feeds stay out of shared caches.
- Permission checks on artists, bulk update requests, flags, network
  bans, feedback, invites and votes on one's own posts.
- Huge searches, tag boxes and form fields are refused before they're
  read, markup is read in linear time, and connections are limited
  (`[server.connections]`).
- `check-config` redacts every credential.

### Upgrading

Some defaults changed (see [Changed
defaults](docs/src/upgrading.md#changed-defaults)):

- Metadata (EXIF and GPS, XMP, IPTC, comments) is removed from new JPEG,
  PNG and WebP originals: `media.strip_metadata` is now `"strip"`. Set
  it to `"off"` to keep originals as uploaded.
- At most 512 connections are served at once, and idle connections are
  closed after 10 seconds (`[server.connections]`).
- `deploy/compose.tiny.yml` publishes port 8080 on `127.0.0.1` only.

Names were made consistent before the 1.0 freeze. If you use these, update
them:

- `/api/v1`: search results' `next` and `previous` are now `next_page` and
  `previous_page`, as in the other lists; tags' `deprecated` is
  `is_deprecated` (also when changing a tag); users' `uploads` and
  `favorites` counts are `upload_count` and `favorite_count`; `/me`'s
  `uploads` is `upload_limits`; creating a saved search answers
  `201 Created`.
- Webhooks: `comment.created` sends the comment's `creator` and
  `creator_url`, as the API does, instead of `author` and `author_url`.
- The moderation log's action names: `pool.undelete` is `pool.restore`,
  `ip.ban` and `ip.unban` are `network.ban` and `network.unban`,
  `tags.mass_update` is `tag.mass_update`, `posts.delete_uploads` is
  `user.delete_uploads`, `posts.purge_batch` is `post.purge_batch`,
  `post_versions.undo` is `user.undo_edits` and `forum_post.unhide` is
  `forum_post.restore`. Existing entries are renamed when you upgrade.
- `comment_count:` and `note_count:` work beside `commentcount:` and
  `notecount:`, and `order:commentcount` and `order:notecount` beside
  `order:comment_count` and `order:note_count`.

## [0.5.0] - 2026-10-02

Moekura now works like Danbooru in most of the ways people notice:
uploading in two steps from a link or a bookmarklet, sources read from
every site Danbooru knows, artist entries and commentary, tag history,
a forum, messages and notifications, and much fuller moderation. Pages
can be translated and themed, and operators get metrics, tracing, stats
and a server that fits in 1 GB. Read the upgrading notes below: some tag
names change, the APIs are now rate limited, and overridden templates
need checking.

### Uploading

- Uploads come in two steps, as on Danbooru: send up to 20 files, a zip
  archive or a link (paste or drop it anywhere on `/uploads/new`), then
  post each file from its own page, which shows what the source says
  (artist, tags, commentary), similar posts and related tags. Links to
  a work of several files fetch them all. `/uploads` lists your files.
- A "Post to …" bookmarklet (`/uploads/bookmarklet`) that keeps the page
  you were on as the referer.
- Warnings before posting: a resized sample instead of the original,
  bad links, files that look AI-generated, and pixel-identical or
  similar images already on the site.
- The tagger's suggestions show while uploading.
- Optional automatic tags from the file (`lowres`, `animated`,
  `wide_image` and so on; off by default), and refusing new uploaders'
  files the tagger flags with tags the site blocks
  (`tagger.new_uploader_blocked`).
- Pixiv ugoira, replacing a post's file, file metadata shown on each
  post, and optionally stripping metadata from originals
  (`media.strip_metadata`).

### Sources and artists

- Links from every site Danbooru supports are recognised, given
  canonical URLs and icons, and read for the artist, tags and
  commentary. Members-only works can be read with `[sources.logins]`.
- Artist entries with other names, URLs and history, found from a
  post's source.
- Artist commentary: the original title and description, with
  translations and history.

### Searching

- Groups with parentheses and `or`: `(cat or dog) -rating:e`.
- New terms: `child:`, `is:`, `has:`, `exif:`, `pixiv:`, `embedded:`,
  `commentary:`, `pixelhash:`, ordering by votes, comments, md5 and
  custom orders, and including deleted posts.
- Search by image with a file, a link or a post (`/iqdb_queries`).
- The blacklist applies inside the search, so pages stay full.
- "Did you mean" suggestions for searches that find nothing.
- Popular, most viewed, top searches and missed searches pages.

### Posts and tagging

- Tag history, sitewide recent changes for wiki, pools and notes, and
  wiki pages with other names.
- Metatags in the tag box (`rating:`, `parent:`, `pool:`, `newpool:`
  and others), related tags while tagging, copying tags from a parent
  or child, and warnings about incomplete tagging.
- Post locks, and undoing a user's edits in bulk.
- Danbooru-style thumbnails with coloured borders for status and
  family; the post's info now sits to the left of the picture, and the
  search bar above the grid.

### Community

- A forum, private messages, mentions and notifications.
- Feedback on users, name changes, invites from the web, and profile
  pictures, banners and bios.
- Comments that don't bump and pinned comments.
- Likely spam from new accounts is held for review.
- Site news shown at the top of every page.

### Moderation

- A moderation page per user with their IP history and staff notes,
  full network bans, blocked email domains and an optional captcha at
  sign-up (`[auth.captcha]`).
- Disapproving pending posts, appeals of deletions, standard reasons,
  and a searchable approval queue with bulk actions. Deleting a post
  needs a reason.
- Deleting all uploads of a user and purging deleted posts in bulk.
- Roles can be created, re-ranked and deleted; staff can only grant
  permissions they hold.
- Paged and rate-limited flag and report queues, and a moderation log
  filtered by user and date that records what changed.

### Users and site

- Colour themes, with a neutral default. Users get safe mode, a time
  zone, large thumbnails, blurring blacklisted posts, custom CSS and
  more settings.
- Pages are translated with Fluent; add or reword languages with
  `paths.locales_override`. See [Translations](docs/src/admin/translations.md).
- A site description, logo, rules page and footer links; tag
  categories managed from the admin panel; robots.txt and a sitemap;
  `/stats`, and activity reports for staff.

### Integrations and API

- Discord webhooks, with their format detected from the URL.
- `/api/v1` and the Danbooru API share a rate limit (300 requests a
  minute, bursts of 60) and send `X-RateLimit-*` headers.
- CORS for configured origins (`[server.cors]`).
- Danbooru apps get uploads by `upload_id`, explore pages, similar
  images, wiki and pool versions and name changes instead of empty
  answers.
- `moekura admin export` writes posts with sidecar metadata.

### Operations

- Prometheus metrics (`telemetry.metrics_bind`) and OTLP tracing.
- Runs on a 1 GB server (`e2e/compose.1gb.yml`; see
  [Hardware](docs/src/install/hardware.md)); `moekura serve` gives
  memory back after bursts.
- Limits on ffmpeg's CPU, memory and threads, and on media tools run
  at once.
- Images are also published for every commit on main (`edge` and
  `git-<sha>`), and the build version shows in the footer.

### Upgrading

- `newpool:`, `commentary:`, `exif:`, `embedded:`, `pixiv:` and
  `pixiv_id:` are now search terms, so tags starting with them are
  renamed to `newpool_…` and so on (or `…_(tag)` if that's taken).
- The APIs are now limited to 300 requests a minute per client. Raise
  `server.api_requests_per_minute` (0 turns it off) if your scripts or
  apps need more.
- Most templates changed (translations and the new layout). If you use
  `paths.templates_override`, compare your copies with the new ones.
- New permissions (`lock_posts`, `undo_edits`, `replace_posts`,
  `send_messages`, `give_feedback`, `invite_users`) go to the built-in
  roles that should have them. Roles you made get none; add them on the
  Roles page.
- If you set `media.allowed_types`, add `ugoira` to accept Pixiv
  ugoira.
- ffmpeg runs are limited to 2 GB of memory by default
  (`media.ffmpeg_memory_mb`); larger files are refused.
- Square thumbnails are gone; a one-off job deletes their files.
  Others run once after the upgrade too: artist URLs are normalised
  (twitter.com becomes x.com) and existing files get pixel hashes.
- The spam filter is on for public sites; tune it under Admin →
  Settings → Spam.
- Deleting a post through the API now needs a `reason`.

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
