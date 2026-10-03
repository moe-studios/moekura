# With Docker Compose

The smallest setup is the app and PostgreSQL on one machine. The
repository's `deploy/compose.tiny.yml` runs both with Docker or Podman
Compose:

```sh
git clone https://github.com/moe-studios/moekura
cd moekura
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > deploy/.env
MOEKURA_BUILD_VERSION="git-$(git rev-parse --short=7 HEAD)" \
  docker compose -f deploy/compose.tiny.yml up -d
curl localhost:8080/readyz   # → ok
```

Open <http://localhost:8080>. The database is migrated automatically on
start.

Two volumes hold everything worth keeping: `db` (PostgreSQL) and `files`
(uploads and thumbnails). Back them up together; see [Backups](../admin/backups.md).

The PostgreSQL settings in the file suit a machine with 1 GB of memory;
[Hardware](hardware.md) has what the stack needs and how to grow it.

The image (about 180 MB) includes its own builds of libvips and ffmpeg with
only the formats Moekura accepts.

To suggest tags for uploads with the [tagger](../admin/tagger.md), add
its compose file:

```sh
MOEKURA_BUILD_VERSION="git-$(git rev-parse --short=7 HEAD)" \
  docker compose -f deploy/compose.tiny.yml -f deploy/compose.tagger.yml up -d
```

## Published images

Images are published to `ghcr.io/moe-studios/moekura` for AMD64 and ARM64:

- `0.5.0` (for example): a release; `latest` tracks stable releases.
- `edge`: the latest published main branch commit.
- `git-<hash>`: a specific commit, using either its seven-character or full hash.

Append `-tagger` to any tag for the tagger image, such as `edge-tagger`.
The page footer shows the release version or `git-<short hash>` embedded in
that build. Main builds do not move `latest`.

The compose files build from source. Since the Docker context excludes `.git`,
the commands above pass the version as a build argument. When building a
release checkout, set `MOEKURA_BUILD_VERSION` to its semver (without `v`).

## Settings

Configure the app with `MOEKURA_*` environment variables in the compose file,
for example:

```yaml
    environment:
      MOEKURA_DATABASE__URL: postgres://moekura:${POSTGRES_PASSWORD}@db/moekura
      MOEKURA_SERVER__PUBLIC_URL: https://booru.example.com
      MOEKURA_MEDIA__MAX_UPLOAD_MB: "200"
```

Every setting is listed under [Configuration](../configuration.md). Put
the site behind a [reverse proxy](reverse-proxy.md) for HTTPS, then
continue with [First steps](first-steps.md).

## Updating

```sh
git pull
MOEKURA_BUILD_VERSION="git-$(git rev-parse --short=7 HEAD)" \
  docker compose -f deploy/compose.tiny.yml up -d --build
```

Migrations run when the new version starts.
