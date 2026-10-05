# Posts and files

## Uploading

Uploading takes two steps, as on Danbooru:

1. **Upload** (`/uploads/new`) takes up to 20 files at once, chosen,
   dropped anywhere on the page or pasted from the clipboard; choosing
   them sends them. Or give it [a link](#uploading-from-a-link): pasting
   one anywhere on the page sends it.
2. Each file then has its own page, showing it with its size and type,
   the posts it [looks like](#duplicates-and-look-alikes), posts from the
   same source, and the form that makes it a post: source (with
   [what it says](#uploading-from-a-link)), rating, tags (with the related
   tags and the artist from the source), parent post, description and the
   artist's commentary, original and translated. An upload of one file
   shows its form straight away; one of several lists them, each linking
   to its page, with **‹ prev** and **next ›** between them.

On the form, **Ctrl+Enter** posts, the tags box counts its tags, and the
form can sit to the right of the file, to its left or below it (**Form:
left · right · below**, or **Shift+L**, **Shift+R** and **Shift+B**),
as wide as you drag the divider; both are remembered. Users whose posts
needn't be approved can still tick **Upload for approval**; those whose
posts wait see how many more they may upload. Until you've posted ten,
the form points to the rules, and the wiki page `help:upload_notice`, if
there is one, is shown as the form's help, open and marked *Updated* when
it changed since your last post.

Under the file are a **Download** link, **Copy ID** (the file's number,
which [Danbooru clients](danbooru-clients.md) post it by), links searching
SauceNAO, Ascii2D, Yandex, Google Lens, Bing and this site for the
picture, and warnings, each linking to its wiki page and explained above
the form:

- **No Source**: the file was sent from disk; if you can, upload the link
  to where you found it instead (the badge searches SauceNAO for it).
- **Bad Source**: the source is an image on a site whose image links
  don't lead back to their page (X's, say): use the page. The wiki page
  is `bad_<site>_link` when there is one (`bad_twitter_link`), else
  `bad_link`.
- **Image Sample**: the file is a resized copy (a Pixiv `img-master`
  file, an X image without `name=orig`): upload the original. The wiki
  page is `<site>_sample` when there is one, else `image_sample`.
- **AI-Generated**: the file's metadata holds an image generator's
  parameters (Stable Diffusion's or NovelAI's).
- **Pixel-Perfect Duplicate**: posts have exactly the same pixels, though
  the file differs (re-encoded, or with its metadata removed); the badge
  links to the post, or a search of them. `pixelhash:<md5>` finds them
  too.

**My uploads** (`/uploads`) lists the files you uploaded, newest first,
linking to their posts once posted, and can be narrowed to those still
downloading, ready or failed, posted or not, of one file type, or from a
source (its start; `*` matches anything). Users who can ban (moderators)
can list anyone's (**Uploader**, blank for everyone's) and look at their
files, but only the uploader can post them. Files not posted within a day
are removed, and you can have at most 250 waiting to be posted (files
that failed in the last hour count too). Without scripts, choose the
files and press **Upload**.

A **zip** of pictures is unpacked into its files, in the order people
sort their names (`2.jpg` before `10.jpg`): at most 100, each within the
upload size limit, none outside the archive (`/…` or `..`); folders,
hidden files and macOS's `__MACOSX` are left out. A zip with Pixiv's
frame data, or only frames numbered as Pixiv numbers them (`000000.jpg`),
is an [ugoira](#ugoira) instead.

Uploading a file that's already a post takes you to that post.

Scripts can still send a whole post in one form to `POST /upload`
(`file` or `url`, `rating`, `tags`, `source`, `parent`, `description`,
`commentary_title` and `commentary_description`), which is also what the
[API](../api.md) does.

## Duplicates and look-alikes

A file that's already a post isn't kept: its page says so, with a link
to that post. When it's a post you can't see (someone else's that was
deleted or waits for approval), it's only refused, without saying which.

A file that only *looks like* posts already on the site (the same
picture resized, recompressed or slightly edited, by
[perceptual hash](search.md#searching-by-image)) shows those posts above
its form. If yours is something else (another version, an edit, the
next page), press **Post anyway**, perhaps making it a child or parent
of the one it resembles. Each says how alike it is; less alike posts
(75–94%) are behind **Show N low similarity matches**. Only posts you can
see and haven't blacklisted are shown, and posts whose files are still
being processed aren't found yet.

**Related posts** lists posts already here from the same source (the
work's page, or its files' links), with a link searching for all of
them.

## Uploading from a link

**…or a link** on the upload form downloads files from the web.
Give it the file itself, or a work's page on one of the
[sites Moekura knows](sources.md): Pixiv, X, Bluesky, DeviantArt, other
boorus, Misskey and some ninety more.

Moekura asks the site for the work's best (original) files and downloads
them, every page of a work of several, each file once. A work of more
than 20 files asks first: tick **Download all N files** and upload it
again to take them all (up to 100). Together they may come to 20 times
the upload size limit, counting what downloads that failed received;
files past that fail. The upload's page shows as soon as the first is
ready (with scripts, the post form follows the moment it is). Each
post's source is the file's own link when that names its work (Pixiv's
`i.pximg.net/…_p3.png`, so the post says which of the work's images it
is), else the work's page. They download in the background, a few at a
time for the whole site and two at a time of each user's (the rest wait
their turn): the upload's page follows them, and a file that can't be
downloaded says why. Any other page whose preview tags (OpenGraph) name
an image works the same way.
What the site says is used as well, and shown under the source field:
the site, the artist and their profiles, the site's tags, and when the
work was published and last changed. **Fetch source data** reads the
source again, after you change it or when the site has changed.

- **The artist**: the tag of the artist whose
  [artist entry](artists.md) lists their profile there is put in the tags
  box; an artist without one gets a link to start it (**Create new
  artist**), filled in with their name and profiles.
- **Tags**: the **Related tags** panel's *From Pixiv* (or the site's
  name) group lists the site's tags as this site's: tags whose wiki
  pages list one as an [other name](wiki.md#other-names), and tags named
  like one or its English translation.
- **Commentary**: the work's title and description fill in the
  [artist's commentary](artists.md#commentary) fields, and become the
  post's commentary if the fields are left empty.

The same happens with files sent with such a link, which becomes their
source (if it's a web link, `http` or `https`), and for uploads through
the APIs (which download only the work's first file). When a site can't
be reached or has changed, the link is downloaded as it is, without
extras.

Links must lead to the public internet: nothing is fetched from
loopback, private, link-local or other special-purpose addresses (for
IPv6, anything outside global unicast `2000::/3` or in its reserved
blocks), whether the link names one, its name resolves to one or a
redirect leads there.

### The bookmarklet

**Bookmarklet** (`/uploads/bookmarklet`) has a **Post to …** link to drag
to the bookmarks toolbar, and lists the sites whose works are read.
Clicking it on a work's page opens the upload page with the page's link
(`/uploads/new?token=…&url=…&ref=…`), which sends it straight away; going
back skips the upload page. `token` is a key of your own, so take the
bookmarklet while logged in, and don't share it: without it, as in a
link to the upload page from another site, the link only fills in the
form, and waits for you to press **Upload**. `ref` is the page you came
from: when the link is a bare image, the work is read from that page if
it's on the same site or shows the image. It's kept as the upload's
`referer_url` (also in the Danbooru API's `upload[referer_url]`).

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
into it. Thumbnails and samples never carry metadata. By default it's
also taken out of JPEG, PNG and WebP originals, below; files of other
types are kept as they were uploaded, so **Download original** still
has whatever they contained.

### Removing metadata from originals

With `strip_metadata` in [`[media]`](../configuration.md#media) set to
`"strip"` (the default) or `"require"`, metadata is taken out of
uploaded originals before they're stored:

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
a message saying so. `"off"` keeps every original exactly as uploaded,
metadata and all.

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
