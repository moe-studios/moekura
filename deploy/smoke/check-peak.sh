#!/bin/sh
# Fails if the tiny stack's `moekura serve` has used 150 MB or more since
# it started (docs/src/install/hardware.md).
set -eu
peak=$("$(dirname "$0")/rss.sh" peak)
echo "app memory at its peak: $peak MB"
if [ "$peak" -ge 150 ]; then
  echo "::error::moekura serve used $peak MB at its peak, over 150 MB"
  exit 1
fi
