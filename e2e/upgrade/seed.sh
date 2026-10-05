#!/bin/sh
# Fills an empty site with a bit of everything, through the CLI and the
# API, and writes what it made to $STATE for check.sh. It only uses what
# the previous release understands, since that's what it runs against
# in the upgrade test.
#
#   BASE_URL=http://localhost:8080 MOEKURA="docker compose … exec -T app moekura" \
#     E2E_ADMIN_NAME=boss E2E_ADMIN_PASSWORD=… STATE=seed.env e2e/upgrade/seed.sh
#
# Needs curl, jq and python3.
set -eu

base=${BASE_URL:-http://localhost:8080}
moekura=${MOEKURA:?set MOEKURA to the command that runs moekura on the site}
admin=${E2E_ADMIN_NAME:?set E2E_ADMIN_NAME}
password=${E2E_ADMIN_PASSWORD:?set E2E_ADMIN_PASSWORD}
state=${STATE:?set STATE to the file to write}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
origin="Origin: $base"

say() { printf '== %s\n' "$*"; }

say users of several roles
printf '%s\n' "$password" | $moekura admin create-user "$admin" --role admin
for user in ursula:moderator junko:janitor alice:member; do
  printf 'password for %s\n' "${user%%:*}" | $moekura admin create-user "${user%%:*}" --role "${user#*:}"
done
$moekura admin settings set site_name '"Upgrade test"'

say an API key
curl -fsS -c "$work/jar" -b "$work/jar" -H "$origin" -o /dev/null \
  --data-urlencode "name=$admin" --data-urlencode "password=$password" "$base/login"
key=$(curl -fsS -b "$work/jar" -H "$origin" -d "name=upgrade test&expires=never" \
  --data-urlencode "password=$password" "$base/settings/api-keys" \
  | grep -o 'mka_[0-9a-f]\{64\}' | head -n 1)
[ -n "$key" ] || { echo "could not create an API key" >&2; exit 1; }

api() {
  method=$1 path=$2
  shift 2
  curl -fsS -X "$method" -H "Authorization: Bearer $key" "$@" "$base/api/v1$path"
}
json() {
  method=$1 path=$2 body=$3
  api "$method" "$path" -H "Content-Type: application/json" -d "$body"
}

say posts
png() {
  # A small PNG in a colour of its own.
  python3 - "$1" > "$work/$1.png" <<'PY'
import struct, sys, zlib
i = int(sys.argv[1])
rgb = bytes([(i * 70) % 256, (i * 130) % 256, (i * 30 + 90) % 256])
raw = b"".join(b"\0" + rgb * 24 for _ in range(16))
def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
sys.stdout.buffer.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 24, 16, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))
PY
}
posts=""
for i in 1 2 3; do
  png "$i"
  # Solid colours look alike to the similarity check.
  id=$(api POST /posts -F "file=@$work/$i.png" -F rating=g -F allow_similar=true \
    -F "tags=upgrade_cat upgrade_$i artist:upgrade_painter" -F "source=https://example.com/art/$i" | jq -r .id)
  posts="$posts $id"
done
set -- $posts
post1=$1 post2=$2 post3=$3

say an alias and an implication
relation() {
  made=$(json POST /tag-relations "{\"kind\": \"$1\", \"antecedent\": \"$2\", \"consequent\": \"$3\"}")
  # Requests from those who may approve them are approved at once.
  if [ "$(echo "$made" | jq -r .status)" = pending ]; then
    json POST "/tag-relations/$(echo "$made" | jq -r .id)/approve" '{}' > /dev/null
  fi
}
relation alias upgrade_kitty upgrade_cat
relation implication upgrade_cat upgrade_animal
# Tagged through the alias.
json PATCH "/posts/$post3" '{"add_tags": ["upgrade_kitty"]}' > /dev/null

say a comment, a note, a pool and a wiki page
comment=$(json POST "/posts/$post1/comments" '{"body": "Seeded before the upgrade."}' | jq -r .id)
note=$(json POST "/posts/$post1/notes" '{"x": 2, "y": 2, "width": 10, "height": 8, "body": "A note"}' | jq -r .id)
pool=$(json POST /pools "{\"name\": \"Upgrade comic\", \"post_ids\": [$post1, $post2, $post3]}" | jq -r .id)
json PUT /wiki-pages/upgrade_cat '{"body": "A [[upgrade_animal]] seeded before the upgrade."}' > /dev/null

say favorites, a vote, a favorite group and a saved search
api PUT "/posts/$post2/favorite" > /dev/null
json PUT "/posts/$post2/vote" '{"score": 1}' > /dev/null
group=$(json POST /favorite-groups "{\"name\": \"upgrade_best\", \"post_ids\": [$post2]}" | jq -r .id)
json POST /saved-searches '{"query": "upgrade_cat", "labels": ["upgrade"]}' > /dev/null

say a webhook, turned off
curl -fsS -b "$work/jar" -H "$origin" -o /dev/null \
  --data-urlencode "url=https://example.com/moekura-upgrade-hook" \
  --data-urlencode "description=Upgrade test" -d "post.created=on&format=moekura" \
  "$base/admin/webhooks"

say waiting for thumbnails
for id in $posts; do
  for _ in $(seq 60); do
    [ "$(api GET "/posts/$id" | jq -r .file.processed)" = true ] && break
    sleep 1
  done
  [ "$(api GET "/posts/$id" | jq -r .file.processed)" = true ] || { echo "post $id was never processed" >&2; exit 1; }
done

cat > "$state" <<EOF
key=$key
post1=$post1
post2=$post2
post3=$post3
comment=$comment
note=$note
pool=$pool
group=$group
EOF
say seeded: posts $posts
