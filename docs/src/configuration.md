# Configuration

There are two kinds of settings:

- **Server configuration**, read at startup: where the database is, where
  files go, limits. Changing it needs a restart. It's described on this
  page.
- **Site settings**, stored in the database and changed while the site
  runs: the site's name, registration, the approval queue, the default
  blacklist. Change them under **Admin → Settings** or with
  `uwubooru admin settings`.

## Where server configuration comes from

Each layer overrides the one before:

1. built-in defaults,
2. `uwubooru.toml` in the working directory, or the file given with
   `--config <path>` or `UWU_CONFIG`,
3. environment variables.

Every key has an environment variable `UWU_<SECTION>__<KEY>` (two
underscores), for example `UWU_DATABASE__URL` or
`UWU_STORAGE__S3__BUCKET`. Lists are written as TOML, e.g.
`UWU_SERVER__TRUSTED_PROXIES='["10.0.0.0/8"]'`. Unknown keys are refused, so
a typo fails loudly instead of being ignored.

`uwubooru check-config` validates the configuration and prints the result
with passwords redacted.

## `[server]`

| Key | Default | Meaning |
|---|---|---|
| `bind` | `"0.0.0.0:8080"` | address the HTTP server listens on |
| `public_url` | `"http://localhost:8080"` | the address people use; set it to your `https://` URL (cookies become `Secure`, and forms are only accepted from this origin) |
| `trusted_proxies` | `[]` | reverse proxies allowed to report the client's address in `X-Forwarded-For` (addresses or CIDR ranges) |
| `request_timeout_secs` | `30` | requests running longer are stopped with a 408 |

## `[database]`

| Key | Default | Meaning |
|---|---|---|
| `url` | *(required)* | the primary's connection URL |
| `replicas` | `[]` | read replicas' URLs, used for searches and listings |
| `max_connections` | `16` | per pool (the primary and each replica) |
| `min_connections` | `0` | |
| `acquire_timeout_secs` | `5` | how long to wait for a free connection |
| `statement_timeout_ms` | `30000` | server-side limit per statement; `0` for none |
| `auto_migrate` | `true` | migrate on start; with several servers, set `false` and run `uwubooru migrate` when deploying |
| `replica_max_lag_secs` | `10` | replicas further behind are skipped until they catch up; also how long someone's reads stay on the primary after they change something |

## `[auth]`

| Key | Default | Meaning |
|---|---|---|
| `session_idle_days` | `30` | a login ends after this many days unused… |
| `session_max_days` | `365` | …or this long after logging in, however active |

## `[jobs]`

| Key | Default | Meaning |
|---|---|---|
| `workers` | `2` | background jobs processed at once, per process |
| `run_in_serve` | `true` | also run workers inside `serve`; set `false` when you run `uwubooru worker` separately |
| `lock_timeout_secs` | `300` | a job whose worker stopped responding is retried after this |

## `[search]`

| Key | Default | Meaning |
|---|---|---|
| `per_page` | `40` | posts per page |
| `max_per_page` | `200` | the most `limit:` may ask for |
| `max_page` | `1000` | deepest numbered page; "next" links keep working beyond it |
| `max_terms` | `40` | most tags and filters in one search |
| `wildcard_limit` | `100` | most tags a wildcard expands to (the most used) |
| `count_limit` | `10000` | result counts are exact up to this, estimated above |
| `count_cost_limit` | `25000` | counts PostgreSQL expects to cost more than this (roughly pages read) are estimated instead, so filters no index covers don't read every post |

## `[storage]`

| Key | Default | Meaning |
|---|---|---|
| `backend` | `"local"` | `"local"` (a directory) or `"s3"` |
| `path` | `"data"` | the directory, for `local` |
| `public_base_url` | *(unset)* | where browsers load files from, e.g. a CDN; unset, the app serves them under `/data/` |

### `[storage.s3]`

| Key | Default | Meaning |
|---|---|---|
| `bucket` | `""` | |
| `region` | `"us-east-1"` | |
| `endpoint` | *(unset)* | for MinIO, Garage, R2, B2 and others; unset for AWS |
| `access_key_id`, `secret_access_key` | `""` | empty to use `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` or instance credentials |
| `path_style` | `false` | put the bucket in the path; most self-hosted stores need this |

See [File storage](admin/storage.md).

## `[media]`

| Key | Default | Meaning |
|---|---|---|
| `max_upload_mb` | `100` | largest upload |
| `max_pixels` | `200000000` | larger images are refused before decoding |
| `max_duration_secs` | `600` | longest video |
| `allowed_types` | `["jpeg", "png", "gif", "webp", "avif", "mp4", "webm"]` | add `"jxl"` for JPEG XL (off by default: libvips doesn't consider its decoder hardened against malicious files) |
| `thumbnail_sizes` | `[250, 500]` | thumbnail boxes, 1x and 2x for high-density screens |
| `sample_size` | `1600` | larger images also get a resized copy for the post page |
| `variant_format` | `"webp"` | `"webp"` or `"avif"` (smaller, slower) for thumbnails and samples |
| `tool_timeout_secs` | `120` | longest a media tool may run |
| `work_dir` | system temp dir | scratch space for uploads and processing |

### `[media.tools]`

Paths to `vips`, `vipsheader`, `vipsthumbnail`, `ffmpeg` and `ffprobe`,
if they aren't on `PATH`.

## `[paths]`

| Key | Meaning |
|---|---|
| `templates_override` | a directory whose files replace built-in templates with the same path, e.g. `base.html` |
| `static_override` | the same for static files, e.g. `css/main.css` |

## `[telemetry]`

| Key | Default | Meaning |
|---|---|---|
| `log_format` | `"text"` | `"text"` or `"json"` |
| `log_filter` | `"info,sqlx::postgres::notice=warn"` | a `tracing` filter; `RUST_LOG` overrides it (`"info,tower_http=debug"` logs every request) |
