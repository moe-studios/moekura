# Stability

From 1.0 on, Moekura follows [semantic versioning](https://semver.org/)
strictly: anything that would break a working site, its scripts or the
apps people use with it waits for the next major release (2.0). This page
says what that covers.

## What 1.0 promises

These keep working, with the same meaning, across every 1.x release.
Things may be **added** to them in a minor release (1.1, 1.2): new
endpoints, fields, keys, flags, metatags or events. Nothing is removed or
renamed except through a [deprecation](#deprecations).

| Surface | Covers |
|---|---|
| [`/api/v1`](api.md) | its endpoints, fields, parameters, error shape, status codes and rate limit headers, as its OpenAPI description at `/api/v1/openapi.json` lists them |
| [File transfers](api.md#large-files) | `/uploads/files` speaking tus 1.0.0 with its creation and termination extensions, and the `transfer` field that names a file sent there |
| The [Danbooru-compatible API](using/danbooru-clients.md) | what gallery-dl, Grabber and Boorusama use, which the tests check on every change; the rest of it follows Danbooru as closely as it can, which may change when Danbooru does |
| [Configuration](configuration.md) | every key in `moekura.toml`, its `MOEKURA_*` variable, its unit and its default's meaning; `MOEKURA_CONFIG`; site settings keys (`moekura admin settings`) |
| [Commands](admin/commands.md) | every `moekura` command, its flags and arguments, and its exit status (`0` for success); the built-in role keys (`member`, `admin`, …) |
| [Search syntax](using/search.md) | tags, `-`, `~`, `or`, groups, wildcards, and every metatag and `order:` value |
| [Webhooks](admin/webhooks.md) | event names, the generic payload's fields, the `X-Moekura-*` headers and the signature; the Discord format's meaning (Discord decides how it looks) |
| Atom feeds | `/posts.atom` and `/comments.atom`, their parameters and feed tokens |
| Page addresses | `/posts/123`, `/posts?tags=…`, `/wiki/…` and the other addresses people link to, bookmark or put in a browser's search bar; one that moves redirects |
| Metrics | the names, labels and meaning of the [metrics](configuration.md#metrics) |
| Container images | the `X.Y.Z`, `X.Y`, `X` and `latest` tags, the `-tagger` variants, the data volume (`/var/lib/moekura/data`, and `/var/lib/moekura/models` in the tagger image) and the user the image runs as |
| Upgrades | any 1.x release upgrades to any later 1.x release by replacing the program and starting it ([Upgrading](upgrading.md)) |

## What isn't a contract

These may change in any release. The changelog says when they do.

- **The database schema.** Only Moekura reads and writes it; migrations
  move it forward on start, and that upgrade path is the contract, not
  the tables. Query the API instead of the database.
- **Overridden templates and static files** (`paths.templates_override`,
  `paths.static_override`). They keep being used, but the built-in ones
  they replace change between releases: check yours against the new
  release's before upgrading. [Themes](admin/themes.md) that only set
  the colour tokens keep working.
- **Translation message ids** (`paths.locales_override`). Ids may be
  added, changed or removed; a missing translation falls back to English,
  so nothing breaks, but check your files after upgrading.
- **Pages' HTML and CSS**, and the scripts on them: scrape the API, not
  the pages.
- **Log messages and their fields**, and the names of trace spans.
  They're for people reading them; alert on [metrics](configuration.md#metrics).
- **Performance and resource use**, beyond what's measured in
  [Hardware](install/hardware.md) and checked in CI.
- **The minimum Rust version** for building from source, which may rise
  in a minor release.
- **Unreleased builds** (`edge`, `git-…`): anything on the main branch may
  still change before it's released.
- **Fixing bugs and security problems** may change behaviour that
  depended on them. When that matters to many sites, the changelog says so.

## What counts as breaking

A change is breaking if a site, script or app that worked before stops
working, or does something different, without anyone changing it.
For example:

- removing or renaming an endpoint, field, parameter, config key,
  variable, flag, command, metatag, event or payload field;
- changing a field's type, or what a value means (a unit, a default that
  changes behaviour, a status code);
- making something optional required, or refusing input that used to be
  accepted;
- needing a step beyond replacing the program to upgrade.

These are not breaking:

- adding endpoints, fields (clients should ignore unknown ones), optional
  parameters, keys with defaults that keep today's behaviour, flags,
  metatags or events;
- adding values to an enumeration, such as a new post status or webhook
  event; clients should handle values they don't know;
- changing the order of JSON fields, the wording of error messages, or
  pages' layout;
- a default that only changes something not covered above, such as the
  thumbnails' look.

## Deprecations

Something that has to be renamed or replaced within 1.x is deprecated
first:

- **Config keys** keep working under their old name, and the program logs
  a warning naming the new one when it starts. Setting both the old and
  the new name is an error.
- **Commands and flags** keep working under their old names, hidden from
  `--help`, with a warning.
- **API endpoints and fields** keep working, are marked `deprecated` in
  the OpenAPI description (with what to use instead), and deprecated
  endpoints answer with a `Deprecation` header
  ([RFC 9745](https://www.rfc-editor.org/rfc/rfc9745)).

The changelog lists every deprecation. Deprecated names are removed only
in the next major release, whose upgrading notes list them all; a site
with no deprecation warnings in its log upgrades without changes to its
configuration. [Development](development.md#renaming-something-public)
says how to add one.
