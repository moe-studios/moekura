#!/bin/sh
# The resident memory of the tiny stack's `moekura serve`, in MB: now, or
# with `peak`, the most it has used since it started.
set -eu
field=VmRSS
[ "${1:-}" = peak ] && field=VmHWM
docker compose -f "$(dirname "$0")/../compose.tiny.yml" exec -T app \
  awk "/^$field:/ { print int(\$2 / 1024) }" /proc/1/status
