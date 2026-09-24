# With Docker Compose

The smallest setup is the app and PostgreSQL on one machine. The
repository's `deploy/compose.tiny.yml` runs both with Docker or Podman
Compose:

```sh
git clone https://github.com/uwuumoe/uwubooru
cd uwubooru
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > deploy/.env
docker compose -f deploy/compose.tiny.yml up -d
curl localhost:8080/readyz   # → ok
```

Open <http://localhost:8080>. The database is migrated automatically on
start.

Two volumes hold everything worth keeping: `db` (PostgreSQL) and `files`
(uploads and thumbnails). Back them up together; see [Backups](../admin/backups.md).

The image includes libvips and ffmpeg, which make up most of its size.

## Settings

Configure the app with `UWU_*` environment variables in the compose file,
for example:

```yaml
    environment:
      UWU_DATABASE__URL: postgres://uwu:${POSTGRES_PASSWORD}@db/uwu
      UWU_SERVER__PUBLIC_URL: https://booru.example.com
      UWU_MEDIA__MAX_UPLOAD_MB: "200"
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
