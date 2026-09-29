#!/bin/sh
# The resident memory of the tiny stack's `moekura serve`, in MB.
set -eu
docker compose -f "$(dirname "$0")/../compose.tiny.yml" exec -T app \
  awk '/^VmRSS:/ { print int($2 / 1024) }' /proc/1/status
