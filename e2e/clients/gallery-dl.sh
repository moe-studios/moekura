#!/bin/sh
# Downloads from a running site with gallery-dl, as a Danbooru instance:
# uploads a few posts, makes an API key, and checks gallery-dl finds every
# post (paging with b<id>) with its tags split by category.
#
#   BASE_URL=http://localhost:8080 E2E_ADMIN_NAME=boss E2E_ADMIN_PASSWORD=… \
#     e2e/clients/gallery-dl.sh
#
# Needs curl, python3 and gallery-dl on the PATH.
set -eu

base=${BASE_URL:-http://localhost:8080}
name=${E2E_ADMIN_NAME:?set E2E_ADMIN_NAME}
password=${E2E_ADMIN_PASSWORD:?set E2E_ADMIN_PASSWORD}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
run=$(date +%s)
tag="gdl_$run"

# Forms are only accepted from the site's own origin.
origin="Origin: $base"
# The browser tests log in as the same account just before; wait out its
# login rate limit rather than fail.
curl -fsS --retry 3 -c jar -b jar -H "$origin" -o /dev/null \
  --data-urlencode "name=$name" --data-urlencode "password=$password" "$base/login"

for i in 1 2 3; do
  # A small PNG in a colour of its own, so reruns don't upload duplicates.
  python3 - "$i" "$run" > "img$i.png" <<'PY'
import struct, sys, zlib
i, run = int(sys.argv[1]), int(sys.argv[2])
rgb = bytes([(run * 7 + i * 60) % 256, (run * 13) % 256, (i * 90) % 256])
raw = b"".join(b"\0" + rgb * 16 for _ in range(16))
def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
sys.stdout.buffer.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 16, 16, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))
PY
  curl -fsS -b jar -H "$origin" -o /dev/null -F "file=@img$i.png" -F rating=g \
    -F "tags=$tag n$i artist:gdl_painter" "$base/upload"
done

key=$(curl -fsS -b jar -H "$origin" -d "name=gallery-dl $run&expires=never" "$base/settings/api-keys" \
  | grep -o 'mka_[0-9a-f]\{64\}' | head -n 1)
[ -n "$key" ] || { echo "could not create an API key" >&2; exit 1; }

# A threshold of 1 makes gallery-dl ask for the next page (page=b<id>)
# after every page that isn't empty.
cat > config.json <<JSON
{"extractor": {"Danbooru": {"threshold": 1}}}
JSON
gallery-dl --config config.json -u "$name" -p "$key" --write-metadata -d out \
  "Danbooru:$base/posts?tags=$tag"

files=$(find out -name '*.png' | wc -l)
[ "$files" -eq 3 ] || { echo "expected 3 files, got $files" >&2; exit 1; }
python3 - out <<'PY'
import json, pathlib, sys
for meta in pathlib.Path(sys.argv[1]).rglob("*.json"):
    post = json.loads(meta.read_text())
    assert post["tags_artist"] == ["gdl_painter"], post["tags_artist"]
    assert post["rating"] == "g", post["rating"]
print("gallery-dl found every post, with its tags")
PY
