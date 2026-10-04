# Backups

A site is its PostgreSQL database plus its stored files. Back up both, the
database first: a file with no database row is harmless, a row whose file
is missing is not.

To copy *posts* to another site, or keep their files with their tags
outside the database, use [`moekura admin export`](import.md#exporting)
instead; it isn't a backup.

CI backs up a site as this page says, restores it into an empty database
and empty storage, and checks that its posts, files and users are intact,
with local storage and with S3.

## The database

```sh
pg_dump --format=custom --file=moekura-$(date +%F).dump moekura
# with compose:
docker compose -f deploy/compose.tiny.yml exec -T db pg_dump -U moekura --format=custom moekura > moekura-$(date +%F).dump
```

The site can keep running while it's dumped.

## Files

Files never change once written, so incremental copies are cheap:

```sh
rsync -a /var/lib/moekura/data/ backup:/srv/moekura-data/
```

With compose, the files are in the `files` volume. Copy them out through
the app's container, as an archive:

```sh
docker compose -f deploy/compose.tiny.yml exec -T app tar -C /var/lib/moekura/data -cf - . > moekura-files-$(date +%F).tar
```

For S3 storage, use the provider's replication or versioning, or copy the
bucket with a tool like [rclone](https://rclone.org/):

```sh
rclone sync s3:moekura /srv/moekura-files
```

## Secrets

The key that signs file links on private sites is stored in the database,
so a database backup includes it. The configuration (`moekura.toml` or the
environment) is in neither backup: keep a copy of it too.

## Restoring

Restore with the release that made the backup, or a newer one: migrations
only move forward, so a newer release upgrades the restored database when
it starts, but an older one can't use it.

1. Stop the app (`moekura serve` and any `moekura worker`).
2. Restore the database into an empty one:

   ```sh
   createdb --owner moekura moekura
   pg_restore --dbname=moekura moekura-2026-10-01.dump
   # with compose, into a new database volume:
   docker compose -f deploy/compose.tiny.yml up -d --wait db
   docker compose -f deploy/compose.tiny.yml exec -T db pg_restore -U moekura --dbname=moekura < moekura-2026-10-01.dump
   ```

3. Put the files back where `storage` points:

   ```sh
   rsync -a backup:/srv/moekura-data/ /var/lib/moekura/data/
   # with compose, into the files volume, as the app's user:
   docker compose -f deploy/compose.tiny.yml run --rm --no-deps -T --entrypoint tar app \
     -C /var/lib/moekura/data -xf - < moekura-files-2026-10-01.tar
   # S3:
   rclone sync /srv/moekura-files s3:moekura
   ```

4. Start the app, then open a few posts to check their files load.

Restoring into a database that isn't empty fails, or mixes the two sites.
Drop it first (`dropdb moekura`), or with compose start from new volumes:
`docker compose -f deploy/compose.tiny.yml down -v` removes both the
database and the files.
