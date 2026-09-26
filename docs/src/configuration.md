# Configuration

There are two kinds of settings:

- **Server configuration**, read at startup: where the database is, where
  files go, limits. Changing it needs a restart. It's described on this
  page.
- **Site settings**, stored in the database and changed while the site
  runs: the site's name, registration, the approval queue, the default
  blacklist. Change them under **Admin → Settings** or with
  `moekura admin settings`.

## Where server configuration comes from

Each layer overrides the one before:

1. built-in defaults,
2. `moekura.toml` in the working directory, or the file given with
   `--config <path>` or `MOEKURA_CONFIG`,
3. environment variables.

Every key has an environment variable `MOEKURA_<SECTION>__<KEY>` (two
underscores), for example `MOEKURA_DATABASE__URL` or
`MOEKURA_STORAGE__S3__BUCKET`. Lists are written as TOML, e.g.
`MOEKURA_SERVER__TRUSTED_PROXIES='["10.0.0.0/8"]'`. Unknown keys are refused, so
a typo fails loudly instead of being ignored.

`moekura check-config` validates the configuration and prints the result
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
| `auto_migrate` | `true` | migrate on start; with several servers, set `false` and run `moekura migrate` when deploying |
| `replica_max_lag_secs` | `10` | replicas further behind are skipped until they catch up; also how long someone's reads stay on the primary after they change something |

## `[auth]`

| Key | Default | Meaning |
|---|---|---|
| `session_idle_days` | `30` | a login ends after this many days unused… |
| `session_max_days` | `365` | …or this long after logging in, however active |

### `[auth.oidc]`

Lets people log in through an OpenID Connect provider (single sign-on):
Authentik, Keycloak, Kanidm, Zitadel, Google and others. Leave the section
out to turn it off.

| Key | Default | Meaning |
|---|---|---|
| `issuer` | *(required)* | the provider's issuer URL, which describes itself at `/.well-known/openid-configuration` under it; `https://`, or `http://` on the same machine |
| `client_id` | *(required)* | from registering Moekura with the provider |
| `client_secret` | *(empty)* | likewise; empty for a public client |
| `button_label` | `"Log in with single sign-on"` | the button on the login page |
| `scopes` | `["openid", "email", "profile"]` | must include `openid` |

Register the redirect URI `https://your.site/login/oidc/callback` (from
`server.public_url`) with the provider. Logins use the authorization code
flow with PKCE.

Someone logging in through the provider for the first time gets a new
account (with no password) when registration is `open`, or one waiting for
approval when it's `approval`; with `invite` or `closed`, only people who
[linked](using/account.md#single-sign-on) an existing account can. A new
account takes its name from the provider (the next free one if it's
taken), and the provider's email address if it says it's verified and
nobody here uses it yet.

## `[cache]`

| Key | Default | Meaning |
|---|---|---|
| `backend` | `"memory"` | where rate limit counters and cached search counts live: `"memory"` (each process on its own) or `"valkey"` (shared by every web server) |
| `url` | *(unset)* | for `valkey`: `redis://host:6379`, or `rediss://` for TLS; Redis and other compatible servers work too |
| `count_ttl_secs` | `30` | how long a search count that reached `search.count_limit` ("10,000+") is reused; `0` turns this off |
| `prefix` | `"moekura"` | starts every key stored in Valkey; give sites that share a server different prefixes |

If Valkey stops answering, each server counts rate limits on its own and
stops caching until it's back, and logs a warning; nothing fails.

## `[jobs]`

| Key | Default | Meaning |
|---|---|---|
| `workers` | `2` | background jobs processed at once, per process |
| `run_in_serve` | `true` | also run workers inside `serve`; set `false` when you run `moekura worker` separately |
| `lock_timeout_secs` | `300` | a job whose worker stopped responding is retried after this |

## `[mail]`

Outgoing mail over SMTP, for email verification and password resets.
Messages are sent by the job workers, so a slow mail server doesn't hold
up the site, and failed sends are retried.

| Key | Default | Meaning |
|---|---|---|
| `host` | *(empty)* | the SMTP server; empty turns mail off, along with the features that need it |
| `tls` | `"starttls"` | `"starttls"` (upgrade a plain connection; required), `"tls"` (TLS from the start) or `"none"` (only for a relay on the same machine or network) |
| `port` | *(by `tls`)* | 587 for `starttls`, 465 for `tls`, 25 for `none` |
| `username`, `password` | *(empty)* | the login, if the server needs one |
| `from` | *(empty)* | the sender, as `address@example.com` or `Site name <address@example.com>`; required with `host` |
| `timeout_secs` | `30` | connecting or sending one message gives up after this |

Check the settings with `moekura admin send-test-mail you@example.com`,
which sends straight away and prints any error.

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

## `[tagger]`

The optional [tagger](admin/tagger.md), which suggests tags for new uploads.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | queue new uploads for `moekura tagger`; set it for every process |
| `model` | `"wd-vit-tagger-v3"` | `wd-vit-tagger-v3`, `wd-convnext-tagger-v3`, `wd-swinv2-tagger-v3`, `wd-eva02-large-tagger-v3`, or `"custom"` |
| `model_url`, `tags_url` | unset | for `custom`: the ONNX model and its `selected_tags.csv` |
| `model_sha256`, `tags_sha256` | unset | for `custom`: their SHA-256 checksums |
| `model_dir` | `"data/models"` | where models are downloaded to, one directory each |
| `runtime` | `ORT_DYLIB_PATH`, or the system's | the ONNX Runtime library, `libonnxruntime.so` |
| `threads` | `0` | threads one image uses; `0` means one per core |
| `workers` | `1` | posts tagged at once |
| `account` | `"tagger"` | who automatically applied tags are credited to; created without a password on first use |

## `[telemetry]`

| Key | Default | Meaning |
|---|---|---|
| `log_format` | `"text"` | `"text"` or `"json"` |
| `log_filter` | `"info,sqlx::postgres::notice=warn,ort=warn"` | a `tracing` filter; `RUST_LOG` overrides it (`"info,tower_http=debug"` logs every request) |

## `[webhooks]`

| Key | Default | Meaning |
|---|---|---|
| `allow_private_addresses` | `false` | let [webhooks](admin/webhooks.md) go to private, loopback and link-local addresses (a service on the same machine or network) |
| `timeout_secs` | `10` | how long a delivery may take |
