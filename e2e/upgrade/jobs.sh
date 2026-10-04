#!/bin/sh
# Waits for the background jobs that are due (such as the one-off jobs
# migrations queue) to finish, and fails if any failed for good.
#
#   PSQL="docker compose … exec -T db psql -U moekura -d moekura" e2e/upgrade/jobs.sh
set -eu

psql=${PSQL:?set PSQL to a psql command for the database}
query() { $psql -tA -c "$1"; }

for _ in $(seq 120); do
  waiting=$(query "SELECT count(*) FROM jobs WHERE status = 'running' OR (status = 'queued' AND run_at <= now())")
  [ "$waiting" -eq 0 ] && break
  sleep 1
done
if [ "$waiting" -ne 0 ]; then
  echo "jobs still waiting after two minutes:" >&2
  query "SELECT kind, status, attempts, last_error FROM jobs WHERE status <> 'dead'" >&2
  exit 1
fi
dead=$(query "SELECT count(*) FROM jobs WHERE status = 'dead'")
if [ "$dead" -ne 0 ]; then
  echo "jobs failed:" >&2
  query "SELECT kind, attempts, last_error FROM jobs WHERE status = 'dead'" >&2
  exit 1
fi
echo "every due job finished"
