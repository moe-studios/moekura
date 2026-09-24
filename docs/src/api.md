# The API

Everything the site does is available as JSON under `/api/v1`. Each site
documents its own version of the API at `/api/docs`, generated from the
code, and serves the OpenAPI description at `/api/v1/openapi.json` for
generating clients (`moekura openapi` prints it too).

## Authenticating

Create a key under **Settings → API keys** and send it with each request:

```sh
curl -H "Authorization: Bearer mka_…" https://booru.example.com/api/v1/me
```

A key acts as you: it can do what your role allows, and while you're
banned only what visitors can. It's shown once, when created; revoke it
from the same page if it leaks. Keys start with `mka_` so that secret
scanners can spot them. Without a key, requests are made as a visitor.

## Errors

Errors are JSON with the HTTP status:

```json
{"error": {"status": 422, "message": "Choose a rating."}}
```

A duplicate upload is a `409` whose error also has `post_id`, the post that
already has the file.

## Examples

Search, 100 posts at a time:

```sh
curl "https://booru.example.com/api/v1/posts?tags=cat+-dog&limit=100"
```

The response has `posts`, a `count`, and `next`: pass it back as `page` for
the next page, until it's absent.

Upload a file:

```sh
curl -H "Authorization: Bearer $KEY" \
  -F file=@cat.png -F rating=g -F "tags=cat artist:someone" \
  https://booru.example.com/api/v1/posts
```

Add and remove tags without touching the others:

```sh
curl -X PATCH -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"add_tags": ["sleeping"], "remove_tags": ["standing"]}' \
  https://booru.example.com/api/v1/posts/123
```
