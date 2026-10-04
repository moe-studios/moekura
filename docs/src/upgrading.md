# Upgrading

Releases are tagged `vX.Y.Z` and listed, with what changed, in
[CHANGELOG.md](https://github.com/moe-studios/moekura/blob/main/CHANGELOG.md).
Each comes as a container image for amd64 and arm64
(`ghcr.io/moe-studios/moekura:X.Y.Z`, also tagged `X.Y`, `X` from 1.0 on,
and `latest`) and as Linux binaries on the release page.

From 1.0 on, versions follow [semantic versioning](https://semver.org/):
a patch release (1.0.0 → 1.0.1) only fixes things, a minor release
(1.0 → 1.1) adds things without breaking existing sites, and only a major
release (2.0) may remove or change what [Stability](stability.md) lists.
Before 1.0, a minor release (0.4 → 0.5) could change configuration or
behaviour, and its changelog says what to do.

## Steps

1. Read the changelog for every release between yours and the new one.
2. [Back up](admin/backups.md) the database.
3. Replace the program: pull the new image, or put the new binary in
   place.
4. Start it. Database migrations run on start unless
   `database.auto_migrate = false`; with several servers, run
   `moekura migrate` once first, then restart them all.

Migrations only move forward. To go back to an older version, restore the
backup you took.

## Within 1.x

Upgrading from any 1.x release to any later 1.x release never needs more
than the steps above, whichever releases are skipped: configuration,
scripts and apps that worked keep working, as [Stability](stability.md)
describes. A minor release may deprecate something; the old name keeps
working, and the log says what to change, so that the next major release
needs no work. Anything that needs more is saved for 2.0, whose changelog
will list what to do.

Overridden templates and translations (`paths.templates_override`,
`paths.locales_override`) aren't covered: compare them with the new
release's before upgrading.

## Supported releases

After 1.0, the latest minor release gets bug and security fixes as patch
releases. Bug fixes aren't backported to older minor releases; security
fixes are, for a while, as [SECURITY.md](https://github.com/moe-studios/moekura/blob/main/SECURITY.md#supported-versions)
says. Every upgrade test in CI starts from the previous release, so
upgrading one release at a time is the best-tested path, but skipping
releases works too.

## Requirements

Moekura is tested with the versions in the container image and the
compose files. The oldest versions supported:

| | Oldest supported | Tested in CI |
|---|---|---|
| PostgreSQL | 16 | 18 |
| Valkey (optional) | Valkey 7.2, or Redis 6.2 | Valkey 8 |
| libvips | 8.15 | 8.16 (Debian 13) and 8.18 (the image) |
| ffmpeg | 7.0 | 7.1 (Debian 13) and 9.0 (the image) |
| glibc, for the release binaries | 2.39 | Ubuntu 24.04 |
| Rust, to build from source | 1.94 | 1.94 and the latest stable |

Raising one of these happens only in a minor release, never in a patch
release, and is announced in the changelog of the minor release before.
A PostgreSQL version stays supported at least until its maintainers stop
supporting it. The container image always ships what it needs, so this
matters only for sites run [without containers](install/bare-metal.md)
or with their own database.

## With Docker Compose

Point the `app` service at a release instead of building it:

```yaml
services:
  app:
    image: ghcr.io/moe-studios/moekura:0.5
```

```sh
docker compose -f deploy/compose.tiny.yml pull
docker compose -f deploy/compose.tiny.yml up -d
```

## Binaries

The Linux binaries are built on Ubuntu 24.04 and need glibc 2.39 or newer
(Debian 13, Ubuntu 24.04, Fedora 40 and later), plus the
[media tools](install/bare-metal.md).
