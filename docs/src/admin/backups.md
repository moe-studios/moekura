# Backups

A site is its PostgreSQL database plus its stored files. Back up both, the
database first: a file with no database row is harmless, a row whose file
is missing is not.

## The database

```sh
pg_dump --format=custom --file=moekura-$(date +%F).dump moekura
# with compose:
docker compose -f deploy/compose.tiny.yml exec -T db pg_dump -U moekura --format=custom moekura > moekura-$(date +%F).dump
```

Restore into an empty database with `pg_restore --dbname=moekura FILE`.

## Files

Files never change once written, so incremental copies are cheap:

```sh
rsync -a /var/lib/moekura/data/ backup:/srv/moekura-data/
```

For S3 storage, use the provider's replication or versioning, or a tool
like `rclone sync`.

## Secrets

The key that signs file links on private sites is stored in the database,
so a database backup includes it.
