# Danbooru apps

Moekura answers Danbooru's API too, so apps and tools made for Danbooru
can use a Moekura site: point them at the site as if it were a Danbooru
instance. Logged out, they see what visitors see. To log in, make an API
key under **Settings → API keys** and give the app your name and the key
(not your password).

## gallery-dl

Put `Danbooru:` in front of a site URL:

```sh
gallery-dl "Danbooru:https://booru.example.com/posts?tags=cat"
gallery-dl -u yourname -p mka_… "Danbooru:https://booru.example.com/posts?tags=cat"
```

Or add the site once in gallery-dl's configuration, and use its URLs as
they are:

```json
{
  "extractor": {
    "Danbooru": {
      "mybooru": { "root": "https://booru.example.com" }
    },
    "mybooru": { "username": "yourname", "password": "mka_…" }
  }
}
```

CI checks gallery-dl against every change.

The apps below aren't tested automatically; if one trips over something,
please [open an issue](https://github.com/moe-studios/moekura/issues/new/choose).

## Grabber

Add a source with the site's address and choose **Danbooru (2.0)** as its
type. In the source's settings, enter your name and your API key under
login.

## Boorusama

Add a booru with the site's address and choose **Danbooru** as its
engine, then log in with your name and your API key.

## What works

| Endpoint | Notes |
|---|---|
| `/posts.json`, `/posts/{id}.json`, `/posts/random.json`, `/counts/posts.json` | search with the site's [syntax](search.md); `page` takes numbers, `b<id>` and `a<id>`; up to 200 per page; `only=` picks fields |
| `PUT /posts/{id}.json` | `post[tag_string]` (with `post[old_tag_string]`), `post[rating]`, `post[source]`, `post[parent_id]` |
| `/post_versions.json` | by `search[post_id]` |
| `/tags.json`, `/autocomplete.json`, `/related_tag.json` | related tags are estimated from a search's newest 200 posts |
| `/tag_aliases.json`, `/tag_implications.json` | |
| `/wiki_pages.json`, `/wiki_pages/{title or id}.json` | |
| `/profile.json`, `/users.json`, `/users/{id}.json` | levels follow roles: Member 20, Contributor 35, Janitor 37, Moderator 40, Admin 50 |
| `/favorites.json`, `/favorites/{post_id}.json`, `/posts/{id}/favorites.json` | |
| `/posts/{id}/votes.json`, `/post_votes.json` | only your own votes are listed |
| `/explore/posts/popular.json` | the best-scored posts of a day, week or month |
| `/comments.json`, `/comments/{id}.json` | listed newest first, by `search[post_id]`, `search[creator_id]` or `search[creator_name]`; `page` takes numbers and `b<id>`; post with `comment[post_id]` and `comment[body]`, and change or delete your own |
| `/comments/{id}/votes.json`, `/comment_votes.json` | only your own votes are listed |
| `/pools.json`, `/pools/{id}.json` | by `search[name_matches]`, `search[name_contains]`, `search[id]` or `search[category]`; `post_ids` lists the posts you can see |
| `/favorite_groups.json`, `/favorite_groups/{id}.json` | by `search[creator_id]` or `search[creator_name]`, otherwise yours |
| `/notes.json`, `/notes/{id}.json`, `/note_versions.json` | notes by `search[post_id]` (one or more posts); versions by `search[post_id]` or `search[note_id]`; read-only |
| `POST /uploads.json`, `/uploads/{id}.json` | the first step of an upload: a file as `upload[files][0]`, or a link as `upload[source]`; its upload, upload media asset and media asset share one id |
| `POST /posts.json` | the second step: `upload_media_asset_id` with `post[tag_string]`, `post[rating]`, `post[source]` and optionally `post[parent_id]`; upload limits apply. Uploads not made into posts within a day are removed |
| `/ai_tags.json` | the [tagger's](../admin/tagger.md) suggestions, newest posts first, by `search[post_id]` (or `search[media_asset_id]`, the same number), `search[tag_name]`, `search[tag_id]`, `search[is_posted]` and `search[score]` (`>=50`, `50..90`); `score` is 0 to 100 |
| `/saved_searches.json` | yours; add with `saved_search[query]` and `saved_search[label_string]`, and delete |

## What doesn't

- Artists, forums and messages don't exist in Moekura: their lists are
  empty, and single ones are "not found".
- Favorites and your votes on posts and comments have no ids of their
  own: their `id` is the post's or comment's.
- Pools, favorite groups and notes are read-only here; change them on
  the site or with [Moekura's API](../api.md).
- Responses are JSON only, not XML.
