# End-to-end tests

A browser (Playwright, Chromium) using a running site the way people do:
registering, uploading, tagging, searching, favoriting, moderating, and
making the site private. CI runs them against `deploy/compose.tiny.yml`.

They need an empty site with open registration and an admin account:

```sh
# With the compose stack (or `moekura serve` against an empty database):
echo "e2e admin password" | docker compose -f deploy/compose.tiny.yml exec -T app \
  moekura admin create-user boss --role admin

cd e2e
npm ci
npx playwright install chromium
E2E_ADMIN_NAME=boss E2E_ADMIN_PASSWORD="e2e admin password" npx playwright test
```

`BASE_URL` points them elsewhere than `http://localhost:8080`.

`clients/gallery-dl.sh` checks the [Danbooru API](../docs/src/using/danbooru-clients.md)
with gallery-dl, against the same site and admin (`BASE_URL`,
`E2E_ADMIN_NAME`, `E2E_ADMIN_PASSWORD`); it needs `gallery-dl` and
`python3`.

`upgrade/` holds the upgrade and backup tests (`.github/workflows/upgrade.yml`):
`seed.sh` fills an empty site through the CLI and the API, `check.sh`
checks it afterwards, and `jobs.sh` waits for background jobs and fails on
any that died. They need `curl`, `jq` and `python3`; see each script for
its variables.
