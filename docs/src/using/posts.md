# Posts and files

## Uploading

Uploading takes two steps, as on Danbooru:

1. **Upload** (`/uploads/new`) takes up to 20 files at once, chosen,
   dropped onto the form or pasted from the clipboard; choosing them
   sends them. Or give it [a link](#uploading-from-a-link).
2. Each file then has its own page, showing it with its size and type,
   the posts it [looks like](#duplicates-and-look-alikes), and the form
   that makes it a post: source, rating, tags (with the related tags and
   the artist from the source), parent post, description and the
   artist's commentary. An upload of one file shows its form straight
   away; one of several lists them, each linking to its page, with
   **‹ prev** and **next ›** between them.

**My uploads** (`/uploads`) lists the files you uploaded, newest first,
linking to their posts once posted. Files not posted within a day are
removed. Without scripts, choose the files and press **Upload**.

Scripts can still send a whole post in one form to `POST /upload`
(`file` or `url`, `rating`, `tags`, `source`, `parent`, `description`,
`commentary_title` and `commentary_description`), which is also what the
[API](../api.md) does.

## Duplicates and look-alikes

A file that's already a post isn't kept: its page says so, with a link
to that post.

A file that only *looks like* posts already on the site (the same
picture resized, recompressed or slightly edited, by
[perceptual hash](search.md#searching-by-image)) shows those posts above
its form. If yours is something else (another version, an edit, the
next page), press **Post anyway**, perhaps making it a child or parent
of the one it resembles. Only posts you can see and haven't blacklisted
are shown, and posts whose files are still being processed aren't found
yet.

## Uploading from a link

**…or a link** on the upload form downloads files from the web.
Give it the file itself, or a work's page on a site Moekura can read:

| Site | Pages |
|---|---|
| Pixiv | `pixiv.net/artworks/<id>`, and its files on `i.pximg.net` |
| X (Twitter) | `x.com/<user>/status/<id>`, and `…/photo/<n>` for one picture |
| Bluesky | `bsky.app/profile/<user>/post/<id>` |
| DeviantArt | `deviantart.com/<user>/art/<work>` |
| pixivFANBOX | `<creator>.fanbox.cc/posts/<id>` (public posts) |
| Skeb | `skeb.jp/@<creator>/works/<n>` |

Moekura asks the site for the work's best (original) files and downloads
them, every page of a work of several (up to 100), and the page, not the
file, becomes each post's source. They download in the background: the
upload's page follows them, and a file that can't be downloaded says
why. Any other
page whose preview tags (OpenGraph) name an image works the same way.
What the site says is used as well:

- **The artist**: beside the tags box, the tag of the artist whose
  [artist entry](artists.md) lists their profile there; an artist without
  one gets a link to start it, filled in with their name and profiles.
- **Tags**: the **Related tags** panel's *From Pixiv* (or the site's
  name) group lists the site's tags as this site's: tags whose wiki
  pages list one as an [other name](wiki.md#other-names), and tags named
  like one or its English translation.
- **Commentary**: the work's title and description fill in the
  [artist's commentary](artists.md#commentary) fields, and become the
  post's commentary if the fields are left empty.

The same happens with files sent with such a link, which becomes their
source, and for uploads through the APIs (which download only the
work's first file). When a site can't be reached or has
changed, the link is downloaded as it is, without extras.

## Ugoira

Pixiv's animations (*ugoira*) are a zip of JPEG or PNG frames. Upload the
zip like any file, or link to the work on Pixiv, which downloads the zip
and keeps each frame's time in it. A zip you upload yourself can say how
long each frame shows in an `animation.json` beside the frames, in
Pixiv's form:

```json
{"frames": [{"file": "000000.jpg", "delay": 100}, {"file": "000001.jpg", "delay": 80}]}
```

Without one, every frame shows for a tenth of a second. Once it's
processed, the post plays the animation as a video made from its frames
(a WebM), and **Download the frames** gets the zip. `filetype:ugoira`
(or `filetype:zip`) finds them, and the Danbooru API gives the video as
the post's large file.

## Replacing a post's file

Staff with **Replace posts' files** (moderators and admins, by default)
can swap a post's file for a better one: a higher resolution, an
uncropped version, a fixed scan. **Replace the file**, below the picture,
takes a file or a link, a reason, and whether to move and resize the notes
to the new size (on by default). The post keeps its id, tags, comments,
notes, pools and favourites; its thumbnails and metadata are made again
from the new file, and the file can't be one another post already has.

**Replacements**, under the picture, lists a post's replacements: when,
by whom and why, and the file before (still downloadable, until the post
is purged) and after. Replacements are recorded in the moderation log,
and Danbooru clients can read them at `/post_replacements.json`.

## Thumbnails

Grids show each post's whole picture, scaled to fit, as on Danbooru.
A coloured border says what to know about the post:

| Border | Means |
|---|---|
| blue | pending approval |
| red | flagged |
| black (white in dark mode) | deleted |
| green | has children |
| yellow | has a parent |

When several apply, the family's colour takes the top and left edges and
the status's the right and bottom; a post with both children and a
parent has green on the top and left and yellow on the right and bottom.
Videos and animations show their length in the corner, with a speaker
when they have sound.

## File metadata

When a file is processed, its metadata is read and kept: the image's
make-up (colour components, bit depth, colour space, frames), EXIF (the
camera, the software, comments), XMP (title, creator tool, keywords), PNG
text chunks, and for videos the container and each stream (codec, frame
rate, bit rate, audio channels). **Metadata**, under the picture on a
post's page, lists it by group, with names like exiftool's: `EXIF:Make`,
`PNG:Software`, `File:ColorComponents`.

Where a photo was taken and whose camera took it stay private: GPS and
other location fields, serial numbers and owners' names are never read
into it. Thumbnails and samples never carry metadata. By default the
original file itself is kept as it was uploaded, so **Download original**
still has whatever it contained; sites can remove it from originals too,
below.

### Removing metadata from originals

With `strip_metadata` in [`[media]`](../configuration.md#media) set to
`"strip"` or `"require"`, metadata is taken out of uploaded originals
before they're stored:

| Type | What's removed | What stays |
|---|---|---|
| JPEG | EXIF (GPS, camera, dates…), XMP, IPTC and other Photoshop data, comments, other application segments, and anything after the picture (extra images some phones append) | JFIF, colour profiles (ICC), Adobe's colour transform |
| PNG | text (`tEXt`, `zTXt`, `iTXt`, which hold XMP), `eXIf`, `tIME`, private chunks, anything after the end | transparency, colour profiles and colour space, gamma, pixel density, animation (APNG) |
| WebP | `EXIF`, `XMP ` and unknown chunks | colour profiles, transparency, animation |

The picture itself isn't re-encoded: its compressed data is copied byte
for byte, so it looks exactly the same and loses no quality. A picture
that's turned by its EXIF orientation keeps the orientation, in an EXIF
block holding nothing else. Other types (GIF, AVIF, JPEG XL, videos,
ugoira) aren't changed by `"strip"`, and are refused by `"require"` with
a message saying so.

Removing metadata changes the file's bytes, so its SHA-256 and MD5 are
those of the stored, cleaned file, as is its storage name. Duplicates are
still found: an upload is a duplicate if either the file as sent or the
file as cleaned is already a post, so uploading the same photo again, or
downloading the original and uploading it, finds the post. This holds for
uploads, links, imports, the APIs and replacements alike. Lookups by the
MD5 of the file as it was elsewhere (Danbooru apps checking whether a file
is here, [`import-remote`](../admin/import.md#from-other-boorus)'s check)
don't match a cleaned file; the file is then downloaded and recognised
as a duplicate when it's cleaned. Posts uploaded before the setting was
turned on keep their originals as they are. The metadata shown on post
pages is read from the stored original, so it only lists what's left.

Posts uploaded before metadata was read get it when their files are
processed again: `moekura admin regenerate-media --all`.
