#!/bin/sh
# rclone in a container, with the S3 stack's SeaweedFS as the `:s3:`
# remote and $BACKUP mounted at /backup.
#
#   BACKUP=/tmp/backup e2e/upgrade/rclone.sh sync :s3:moekura /backup/files
set -eu
exec docker run --rm --network host -v "${BACKUP:?set BACKUP to a directory}:/backup" \
  -e RCLONE_S3_PROVIDER=SeaweedFS -e RCLONE_S3_ENDPOINT=http://localhost:8333 \
  -e RCLONE_S3_ACCESS_KEY_ID=test -e RCLONE_S3_SECRET_ACCESS_KEY=test \
  docker.io/rclone/rclone:1.71 "$@"
