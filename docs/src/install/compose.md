# With Docker Compose

The smallest setup is the app and PostgreSQL on one machine. The
repository's `deploy/compose.tiny.yml` runs both with Docker or Podman
Compose:

```sh
git clone https://github.com/moe-studios/moekura
cd moekura
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > deploy/.env
docker compose -f deploy/compose.tiny.yml up -d
curl localhost:8080/readyz   # → ok
```

Open <http://localhost:8080>. The database is migrated automatically on
start.

Two volumes hold everything worth keeping: `db` (PostgreSQL) and `files`
(uploads and thumbnails). Back them up together; see [Backups](../admin/backups.md).

The image (about 170 MB) includes its own builds of libvips and ffmpeg with
only the formats Moekura accepts.

To suggest tags for uploads with the [tagger](../admin/tagger.md), add
its compose file:

```sh
docker compose -f deploy/compose.tiny.yml -f deploy/compose.tagger.yml up -d
```

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
docker compose -f deploy/compose.tiny.yml up -d --build
```

Migrations run when the new version starts.
