# Posts and files

## Uploading from a link

**…or a link to it** on the upload form downloads a file from the web.
Give it the file itself, or a work's page on a site Moekura can read:

| Site | Pages |
|---|---|
| Pixiv | `pixiv.net/artworks/<id>`, and its files on `i.pximg.net` |
| X (Twitter) | `x.com/<user>/status/<id>`, and `…/photo/<n>` for one picture |
| Bluesky | `bsky.app/profile/<user>/post/<id>` |
| DeviantArt | `deviantart.com/<user>/art/<work>` |
| pixivFANBOX | `<creator>.fanbox.cc/posts/<id>` (public posts) |
| Skeb | `skeb.jp/@<creator>/works/<n>` |

Moekura asks the site for the work's best (original) file and downloads
that, and the page, not the file, becomes the post's source. Any other
page whose preview tags (OpenGraph) name an image works the same way.
What the site says is used as well:

- **The artist**: beside the tags box, the tag of the artist whose
  [artist entry](artists.md) lists their profile there; an artist without
  one gets a link to start it, filled in with their name and profiles.
- **Tags**: the **Related tags** panel's *From Pixiv* (or the site's
  name) group lists the site's tags as this site's: tags whose wiki
  pages list one as an [other name](wiki.md#other-names), and tags named
  like one or its English translation.
- **Commentary**: the work's title and description become the post's
  [artist's commentary](artists.md#commentary), unless you wrote one in
  the form.

The same happens with a file and a **Source** that is such a page, and
for uploads through the APIs. When a site can't be reached or has
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
into it. The original file itself is kept as it was uploaded, so
**Download original** still has whatever it contained.

Posts uploaded before metadata was read get it when their files are
processed again: `moekura admin regenerate-media --all`.
