#!/bin/sh
# Starts the S3 stack's SeaweedFS (e2e/upgrade/compose.s3.yml) and makes
# the site's bucket.
set -eu
compose=${COMPOSE:-docker compose -f deploy/compose.tiny.yml -f e2e/upgrade/compose.s3.yml}
$compose up -d s3
for _ in $(seq 60); do curl -s -o /dev/null http://localhost:8333/ && break; sleep 1; done
curl -fsS -X PUT http://localhost:8333/moekura
