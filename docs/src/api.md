# The API

Everything the site does is available as JSON under `/api/v1`. Each site
documents its own version of the API at `/api/docs`, generated from the
code, and serves the OpenAPI description at `/api/v1/openapi.json` for
generating clients (`moekura openapi` prints it too).

Apps made for Danbooru can use the site too; see [Danbooru apps](using/danbooru-clients.md).

## Authenticating

Create a key under **Settings → API keys** and send it with each request:

```sh
curl -H "Authorization: Bearer mka_…" https://booru.example.com/api/v1/me
```

A key acts as you: it can do what your role allows, and while you're
banned only what visitors can. It's shown once, when created; revoke it
from the same page if it leaks. Keys start with `mka_` so that secret
scanners can spot them. Without a key, requests are made as a visitor.

Making a key asks for your password. Accounts made through single
sign-on have none to give, so for them it works only within 10 minutes
of logging in. A key can't manage keys or the feed token,
or change your address, password, two-factor login or single sign-on
links: those need you logged in on the site. Resetting your password
revokes all your keys, and changing it does too unless you untick that.

## Errors

Errors are JSON with the HTTP status:

```json
{"error": {"status": 422, "message": "Choose a rating."}}
```

A duplicate upload is a `409` whose error also has `post_id`, the post that
already has the file.

An upload that looks like posts already on the site (see
[Duplicates and look-alikes](using/posts.md#duplicates-and-look-alikes))
is a `409` too, with `similar`, those posts closest first, and `staged`,
the number of the file kept meanwhile:

```json
{"error": {"status": 409, "message": "This file looks like posts…", "similar": [12], "staged": 3}}
```

To post it anyway, send the same fields again with `staged=3` instead of
the file, or the file with `allow_similar=true`. Automated uploaders that
check for themselves can always send `allow_similar=true`.

## Rate limits

Each client may make a few hundred requests a minute (300 by default,
with bursts of up to 60; the site's admins set
[`api_requests_per_minute` and `api_burst`](configuration.md#server)).
Requests with a key count against its account, requests without one
against the address they come from. The Danbooru-compatible API shares
the same allowance. Every response says what's left:

| Header | Meaning |
|---|---|
| `X-RateLimit-Limit` | requests that may be made at once (the burst) |
| `X-RateLimit-Remaining` | requests left right now |
| `X-RateLimit-Reset` | when the full burst is available again, in Unix time |

Past the limit, requests get a `429` with `Retry-After`, the seconds to
wait. Some actions also have their own, tighter limits, the same as on the
site: logging in, posting comments, flagging and reporting, and forms that
send email.

## Browser apps on other websites

Browsers only let a script on another website call the API if the site
allows that website. Admins list the allowed origins in
[`[server.cors]`](configuration.md#servercors); by default none are, and
only the site's own pages can call it from a browser.

From an allowed origin, scripts can use `/api/v1` and the
Danbooru-compatible API with any method, sending `Authorization` and
`Content-Type` headers, and can read the rate-limit headers, `Retry-After`,
`Location` and `X-Request-Id` in answers. Authenticate with an API key:

```js
const response = await fetch("https://booru.example.com/api/v1/me", {
  headers: { Authorization: `Bearer ${apiKey}` },
});
```

Cookies are ignored on these requests, so an app can't act through the
visitor's login on the site, unless the admins turn on
`allow_credentials` for origins they trust. Then a script can send
`credentials: "include"` to act as whoever is logged in; the site's
cookies are `SameSite=Lax`, so that only works for origins on the same
site (another subdomain of the same domain). A private site still
refuses requests without a key or login, wherever they come from.

## Examples

Search, 100 posts at a time:

```sh
curl "https://booru.example.com/api/v1/posts?tags=cat+-dog&limit=100"
```

The response has `posts`, a `count`, and `next_page`: pass it back as `page` for
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

Change a wiki page, refusing if someone else changed it since version 3:

```sh
curl -X PUT -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"body": "A small [[animal]].", "base_version": 3}' \
  https://booru.example.com/api/v1/wiki-pages/cat
```

Comment on a post (accounts new enough that the site asks them for a
captcha also send a solved one's token as `"captcha"`):

```sh
curl -X POST -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"body": "Lovely colours."}' \
  https://booru.example.com/api/v1/posts/123/comments
```

Make a pool of three posts, then add a fourth at the end:

```sh
curl -X POST -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"name": "My comic", "category": "series", "post_ids": [120, 121, 122]}' \
  https://booru.example.com/api/v1/pools
curl -X POST -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"post_id": 123}' \
  https://booru.example.com/api/v1/pools/1/posts
```

Add a note (the box is in the original image's pixels):

```sh
curl -X POST -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"x": 120, "y": 40, "width": 200, "height": 80, "body": "Good morning!"}' \
  https://booru.example.com/api/v1/posts/123/notes
```

Saved searches (`/saved-searches`) and favorite groups
(`/favorite-groups`) work the same way; the site's `/api/docs` lists
every endpoint.
