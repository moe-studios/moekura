# Upgrading

Releases are tagged `vX.Y.Z` and listed, with what changed, in
[CHANGELOG.md](https://github.com/uwuumoe/uwubooru/blob/main/CHANGELOG.md).
Each comes as a container image for amd64 and arm64
(`ghcr.io/uwuumoe/uwubooru:X.Y.Z`, also tagged `X.Y` and `latest`) and as
Linux binaries on the release page.

Before 1.0, a minor release (0.1 → 0.2) may change configuration or
behaviour; its changelog says what to do. Patch releases (0.1.0 → 0.1.1)
only fix things.

## Steps

1. Read the changelog for every release between yours and the new one.
2. [Back up](admin/backups.md) the database.
3. Replace the program: pull the new image, or put the new binary in
   place.
4. Start it. Database migrations run on start unless
   `database.auto_migrate = false`; with several servers, run
   `uwubooru migrate` once first, then restart them all.

Migrations only move forward. To go back to an older version, restore the
backup you took.

## With Docker Compose

Point the `app` service at a release instead of building it:

```yaml
services:
  app:
    image: ghcr.io/uwuumoe/uwubooru:0.1
```

```sh
docker compose -f deploy/compose.tiny.yml pull
docker compose -f deploy/compose.tiny.yml up -d
```

## Binaries

The Linux binaries are built on Ubuntu 24.04 and need glibc 2.39 or newer
(Debian 13, Ubuntu 24.04, Fedora 40 and later), plus the
[media tools](install/bare-metal.md).
