# Bulk import

`moekura admin import` adds a folder of images and videos as posts,
taking their tags from the files downloaders and tag managers write next to
them:

```sh
moekura admin import ~/downloads/art --uploader yourname --rating s
# with compose, mount the folder into the container first:
docker compose -f deploy/compose.tiny.yml run --rm -v ~/downloads/art:/import:ro \
  app admin import /import --uploader yourname --rating s
```

| Option | Meaning |
|---|---|
| `--uploader NAME` | the account the posts are uploaded by (required) |
| `--rating R` | rating for files whose sidecar has none: `g`, `s`, `q` or `e` |
| `--tags "a b"` | tags to add to every file |
| `-r`, `--recursive` | also import files in subfolders |
| `--dry-run` | show what would happen, without importing anything |

Every file goes through the same checks as an upload: supported types
only, within the size and pixel limits. Files already on the site are
skipped rather than refused, so if an import stops halfway, run it again.
Each file is reported as it goes, then a summary; the command fails if any
file couldn't be imported. Thumbnails are made afterwards by the job
workers, as for uploads.

## Sidecar files

For `pic.png`, the importer reads `pic.png.json` or `pic.json`, and
`pic.png.txt` or `pic.txt`; when there are both a JSON and a text file,
their tags are combined.

**Text files** list tags one per line, as gallery-dl and Hydrus write them
(spaces within a line become underscores), or on a single line separated by
commas or spaces:

```text
long hair
creator:some artist
series:some show
rating:safe
```

Namespaces are mapped to categories: `creator:` and `artist:` to artist,
`series:` and `copyright:` to copyright, `character:` and `meta:` as they
are. `rating:` sets the rating; `safe` counts as general. Other namespaces
stay part of the tag's name.

**JSON files** are objects with any of:

| Field | Meaning |
|---|---|
| `tags` | a list of tags, a string of them separated by spaces, or an object of lists by category (`{"artist": ["someone"], "general": ["cat"]}`) |
| `tag_string`, `tag_string_general`, `tag_string_artist`, `tag_string_copyright`, `tag_string_character`, `tag_string_meta` | Danbooru's fields, as its API and gallery-dl's Danbooru metadata have them |
| `rating` | `g`, `s`, `q`, `e`, their names, or `safe` |
| `source` | where the file came from |
| `description` | |

Tags that aren't valid here (with `*`, or starting with a search prefix
like `user:`) are left out with a note; the rest of the file is still
imported.

## From other boorus

`moekura admin import-remote` copies posts from another booru through its
public API, with their tags, rating and source, to move a collection
here or keep a copy of a tag:

```sh
moekura admin import-remote https://danbooru.donmai.us "some_artist" --uploader boss
moekura admin import-remote https://yande.re "rating:s cat" --uploader boss --limit 500
```

| Site | `--kind` |
|---|---|
| Danbooru, and other Moekura sites | `danbooru` |
| e621, e926 | `e621` |
| Gelbooru, Safebooru, Rule34 and other Gelbooru 0.2 sites | `gelbooru` |
| Moebooru: Konachan, yande.re | `moebooru` |

The kind is guessed for the sites named; give `--kind` for others. The
search is in the other site's syntax. Posts are imported newest first:

- Tags keep their categories where the site has them (Danbooru and
  e621); from other sites, new tags are general. Tags that aren't valid
  here are left out.
- The source is the post's own source, or its page on the other site.
- Files already here (by MD5, or the same file) aren't downloaded again;
  parents and children are linked as both arrive.
- `--notes` and `--pools` also bring notes and pools from Danbooru-style
  sites. Pools are matched by name.

It waits a second between requests (`--delay-ms`); please don't set it
much lower on other people's sites. Some sites want an account for their
API or to show some files: give `--login` and `--api-key` (Gelbooru:
your user id and API key). Posts whose file the site hides are skipped.

Progress is saved as it goes. Running the same site and search again
carries on where it stopped (after `--limit`, a failure or Ctrl-C), and
once it's finished there's nothing to do; `--restart` starts from the
newest posts again, which also picks up posts added since. To import from
a site on your own network, add `--allow-private-addresses`.
