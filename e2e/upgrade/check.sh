#!/bin/sh
# Checks that what seed.sh made is still there and works: after an
# upgrade, or on a site restored from a backup.
#
#   BASE_URL=http://localhost:8080 E2E_ADMIN_NAME=boss E2E_ADMIN_PASSWORD=… \
#     STATE=seed.env e2e/upgrade/check.sh
#
# Needs curl and jq.
set -eu

base=${BASE_URL:-http://localhost:8080}
admin=${E2E_ADMIN_NAME:?set E2E_ADMIN_NAME}
password=${E2E_ADMIN_PASSWORD:?set E2E_ADMIN_PASSWORD}
. "$(realpath "${STATE:?set STATE to the file seed.sh wrote}")"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
origin="Origin: $base"
failures=0

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  failures=$((failures + 1))
}
# expect DESCRIPTION ACTUAL EXPECTED
expect() {
  if [ "$2" = "$3" ]; then printf 'ok: %s\n' "$1"; else fail "$1: got '$2', expected '$3'"; fi
}
api() {
  curl -fsS -H "Authorization: Bearer $key" "$base/api/v1$1"
}
# status PATH: the HTTP status of a page, logged in.
status() {
  curl -sS -o /dev/null -w '%{http_code}' -b "$work/jar" "$base$1"
}

# The seeded API key still works.
expect "API key" "$(api /me | jq -r .name)" "$admin"
for user in ursula:moderator junko:janitor alice:member; do
  expect "role of ${user%%:*}" "$(api "/users/${user%%:*}" | jq -r .role_key)" "${user#*:}"
done

# Searches, through the alias and the implication.
ids() { api "/posts?tags=$1" | jq -c '[.posts[].id] | sort'; }
all=$(printf '%s\n' "$post1" "$post2" "$post3" | jq -sc 'sort')
expect "search by tag" "$(ids upgrade_cat)" "$all"
expect "search by implied tag" "$(ids upgrade_animal)" "$all"
expect "search by alias" "$(ids upgrade_kitty)" "$all"
expect "search by artist" "$(ids artist:upgrade_painter)" "$all"
expect "search by user" "$(ids "user:$admin+upgrade_2")" "[$post2]"
expect "search by favorite" "$(ids "fav:$admin")" "[$post2]"
expect "search by pool" "$(ids "pool:$pool")" "$all"
expect "search by favorite group" "$(ids "favgroup:$group")" "[$post2]"
expect "search by saved search" "$(ids "search:upgrade")" "$all"

# Posts, their files and thumbnails.
post=$(api "/posts/$post2")
expect "tags" "$(echo "$post" | jq -c '[.tags[].name] | map(select(startswith("upgrade"))) | sort')" \
  '["upgrade_2","upgrade_animal","upgrade_cat","upgrade_painter"]'
expect "source" "$(echo "$post" | jq -r .source)" "https://example.com/art/2"
expect "score" "$(echo "$post" | jq -r .score)" 1
expect "favorites" "$(echo "$post" | jq -r .fav_count)" 1
for url in $(echo "$post" | jq -r '.file.url, .variants[].url'); do
  expect "file $url" "$(curl -sS -o /dev/null -w '%{http_code}' "$url")" 200
done

expect "comment" "$(api "/comments/$comment" | jq -r .body)" "Seeded before the upgrade."
expect "note" "$(api "/notes/$note" | jq -r .body)" "A note"
expect "pool" "$(api "/pools/$pool" | jq -c .post_ids)" "[$post1,$post2,$post3]"
expect "wiki page" "$(api /wiki-pages/upgrade_cat | jq -r .body)" \
  "A [[upgrade_animal]] seeded before the upgrade."
expect "alias" "$(api "/tag-relations?kind=alias&name=upgrade_kitty" | jq -r '.relations[0].status')" active

# Logging in, and the pages.
curl -fsS -c "$work/jar" -b "$work/jar" -H "$origin" -o /dev/null \
  --data-urlencode "name=$admin" --data-urlencode "password=$password" "$base/login"
for path in / "/posts?tags=upgrade_cat" "/posts/$post1" "/pools/$pool" /wiki/upgrade_cat \
  /users/alice /comments /tags /admin/webhooks /moderation/log "/posts.atom?tags=upgrade_cat"; do
  expect "page $path" "$(status "$path")" 200
done
curl -fsS -b "$work/jar" "$base/admin/webhooks" | grep -q "moekura-upgrade-hook" \
  && echo "ok: webhook" || fail "the webhook is missing"
curl -fsS "$base/" | grep -q "Upgrade test" \
  && echo "ok: site name" || fail "the site name setting is lost"

if [ "$failures" -gt 0 ]; then
  echo "$failures checks failed" >&2
  exit 1
fi
echo "all checks passed"
