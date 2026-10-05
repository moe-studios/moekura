# Search syntax

Type tags and filters into the search box, separated by spaces. Tags are
case-insensitive, and spaces inside a tag are written as underscores
(`long_hair`).

## Tags

| You type | Finds posts that… |
|---|---|
| `cat` | have the tag `cat` |
| `cat cute` | have both tags |
| `cat -dog` | have `cat` but not `dog` |
| `~cat ~dog` | have `cat`, `dog` or both |
| `long_*` | have any tag starting with `long_` |
| `*_hair` | have any tag ending in `_hair` |
| `-*_hair` | have no tag ending in `_hair` |

The `~` terms form one group: `a ~b ~c` means `a`, plus at least one of `b`
and `c`. A wildcard stands for the (up to 100) most used tags it matches.

Searches follow tag aliases: if `kitty` is aliased to `cat`, searching for
`kitty` finds posts tagged `cat`. Category prefixes are ignored, so
`artist:someone` searches for `someone`.

## Groups and `or`

Parentheses group terms, and `or` between two terms or groups means
either of them. Filters work inside groups too.

| You type | Finds posts that… |
|---|---|
| `(cat or dog) -rating:e` | have `cat` or `dog`, and aren't explicit |
| `(cat cute) or (dog rating:g)` | have `cat` and `cute`, or are general and have `dog` |
| `-(cat dog)` | don't have both `cat` and `dog` |
| `cat (user:alice or score:>10)` | have `cat`, and were uploaded by alice or score above 10 |

Terms side by side bind tighter than `or`, so `a b or c` means `(a b) or
c`. `~` is shorthand for `or`: `~a ~b` is `(a or b)`, and inside a group
the `~` terms form an `or` of that group. Groups can be nested up to 10
deep, and every tag and filter in them counts towards the site's limit on
terms. Whatever that limit, a search can be at most 1000 words and 10,000
characters long.

A `(` at the start of a word opens a group, and a `)` at the end of a word
closes one, unless it belongs to the tag: `(ganyu_(genshin_impact) or
klee_(genshin_impact))` works as expected. To search for a tag that starts
with `(`, put a category in front of it (`general:(tag)`). `order:`,
`limit:`, `ordfav:` and the other orders apply to the whole search, so they
can't go inside a group or next to `or`.

## Filters

Filters look like `name:value`. Put `-` in front of one to exclude what it
matches (`-rating:e`); `order:` and `limit:` can't be excluded.

| Filter | Example | Meaning |
|---|---|---|
| `rating:` | `rating:e,q` | rating `g`eneral, `s`ensitive, `q`uestionable or `e`xplicit (letters or names, comma-separated) |
| `score:` | `score:>=10` | score |
| `favcount:` | `favcount:>5` | number of favourites |
| `commentcount:`, `comment_count:` | `commentcount:>0` | number of comments |
| `notecount:`, `note_count:` | `notecount:>0` | number of notes |
| `note:` | `note:good_morning` | notes contain these words (underscores for spaces) |
| `id:` | `id:1000..2000` | post number |
| `user:` | `user:alice` | uploaded by this user |
| `fav:` | `fav:alice` | favorited by this user |
| `approver:` | `approver:alice`, `approver:any`, `approver:none` | approved by this user, by anyone, or by no one (posts that never waited for approval) |
| `commenter:` | `commenter:alice` | has a comment by this user |
| `comment:` | `comment:nice_art` | comments contain these words (underscores for spaces) |
| `commentary:` | `commentary:true`, `commentary:untranslated`, `commentary:new_work` | has [artist's commentary](artists.md#commentary) (`true`), none (`false`), a translation (`translated`), an original without one (`untranslated`), or commentary containing these words |
| `exif:` | `exif:file:colorcomponents=1`, `exif:exif:model=canon_eos`, `exif:png:parameters` | the file's [metadata](posts.md#file-metadata) has this field (`group:tag`), with this value if `=value` is given; regardless of case, underscores for spaces. Every field on a post's **Metadata** page links to its search |
| `pixiv:`, `pixiv_id:` | `pixiv:123456`, `pixiv:any`, `pixiv:none`, `pixiv_id:>1000` | the source is this Pixiv work (a number or a range like other numbers), any Pixiv work, or none; works' pages and their files on `i.pximg.net` both count |
| `embedded:` | `embedded:true` | the post's notes are drawn on the picture (see [Notes](notes.md)), or not |
| `noter:` | `noter:alice` | has a note this user wrote or edited |
| `upvote:`, `downvote:` | `upvote:alice` | voted up / down by this user; votes are private, so only staff who review posts may search for others' votes, everyone else only for their own |
| `flagger:` | `flagger:alice` | flagged by this user; only staff who review posts may search for others' flags, everyone else only for their own |
| `search:` | `search:all`, `search:artists` | the newest posts (500 each) of your [saved searches](account.md#saved-searches), all or those with a label |
| `favgroup:` | `favgroup:best`, `favgroup:7`, `favgroup:any`, `favgroup:none` | in one of your favorite groups (by name), or any public group (by number); in any or none of your groups |
| `pool:` | `pool:my_comic`, `pool:12`, `pool:any`, `pool:none` | in this pool (by name or number), in any pool, or in none |
| `width:`, `height:` | `width:>=1920` | size in pixels |
| `mpixels:` | `mpixels:>2` | megapixels (width × height ÷ 1,000,000) |
| `ratio:` | `ratio:16:9`, `ratio:<1` | width ÷ height (`16:9` or a number; exact values match within 0.01) |
| `filesize:` | `filesize:>2mb` | file size, in bytes or with `kb`, `mb`, `gb` (an exact size with a unit matches within 5%) |
| `duration:` | `duration:>30` | length of a video, in seconds |
| `filetype:` | `filetype:png,webm` | file type: `jpg`, `png`, `gif`, `webp`, `avif`, `jxl`, `mp4`, `webm`, `ugoira` (or `zip`) |
| `date:` | `date:2026-01` | upload date (UTC): a day, month or year |
| `source:` | `source:https://twitter.com/foo`, `source:*pixiv.net*`, `source:none`, `source:any` | the source starts with this, or matches a pattern with `*`, regardless of case; or posts without / with a source |
| `age:` | `age:<1w`, `age:2d..1mo` | uploaded this long ago: `<1w` is less than a week ago; units `s`, `mi`, `h`, `d`, `w`, `mo` (30 days; `m` works too) and `y` |
| `updated:` | `updated:<1d`, `updated:2026-01` | last changed (tags, rating, source, status, …) this long ago, or on these days |
| `md5:` | `md5:d41d8cd9…` | the file's MD5 hash |
| `pixelhash:` | `pixelhash:9e107d9d…` | the MD5 of the image's decoded pixels: the same picture in any file |
| `similar:` | `similar:123` | looks like post 123 (the post included); found once files are processed |
| `parent:` | `parent:123`, `parent:none`, `parent:any` | a post and its children, posts without a parent, or posts with one |
| `child:` | `child:any`, `child:none` | posts with children (that aren't deleted), or without |
| `tagcount:` | `tagcount:<5` | number of tags |
| `<category>tags:` | `arttags:0`, `gentags:>20` | number of tags in a category: the category's name followed by `tags` (`artisttags:`, `charactertags:`), or Danbooru's `gentags:`, `arttags:`, `copytags:`, `chartags:` and `metatags:`; `arttags:0` finds posts missing an artist |
| `ai:` | `ai:long_hair` | the [tagger](tags.md#suggestions-from-the-tagger) suggests this tag, and the post doesn't have it yet |
| `status:` | `status:deleted` | `pending`, `active`, `flagged`, `deleted`, `modqueue`, `unmoderated`, `appealed` or `any` (see below) |

`is:` and `has:` are shorthands for other filters, as on Danbooru:

| Shorthand | Same as |
|---|---|
| `is:parent`, `has:children` | `child:any` |
| `is:child`, `has:parent` | `parent:any` |
| `is:sfw`, `is:nsfw` | `rating:g,s`, `rating:q,e` |
| `is:general`, `is:explicit`, … | `rating:g`, `rating:e`, … |
| `is:pending`, `is:deleted`, … | `status:pending`, `status:deleted`, … |
| `has:source` | `source:any` |
| `has:pools` | `pool:any` |
| `has:notes`, `has:comments` | `notecount:>0`, `commentcount:>0` |

Numbers (and sizes and dates) can be compared:

| Form | Meaning |
|---|---|
| `5` | exactly 5 |
| `>5`, `>=5`, `<5`, `<=5` | more / at least / less / at most |
| `5..10` | from 5 to 10, both included |
| `5..`, `..10` | at least 5 / at most 10 |
| `1,2,3` | any of these |

Dates take the same forms: `date:2026-01-31`, `date:>=2026-01`,
`date:2025..2026` (all of 2025 and 2026).

A site can limit the ratings logged-out visitors see (**Admin →
Settings → Ratings visitors see**). Their searches, post pages, feeds and
API results then leave out other ratings, whatever the search asks for.

### Statuses

Searches show active and flagged posts, plus your own uploads that are
waiting for approval. Staff who review uploads also see pending posts.
Deleted posts only appear with `status:deleted` or `status:any` (or with a
`status:` inside a group, such as `(status:deleted or rating:e)`), and only
to those allowed to see them. They see how many deleted posts a search
left out, with a link to include them, or can include them in every
search with **Include deleted posts in searches** in their settings. For staff who review uploads,
`status:unmoderated` finds the pending posts left for them: ones they
didn't upload and haven't disapproved; `status:appealed` finds deleted
posts with an open appeal. `status:modqueue` finds everything waiting for
a moderator: pending and flagged posts.

## When nothing is found

If a search finds nothing, tags in it that match no posts get
suggestions: the tag a retired alias pointed to, or used tags spelled
almost the same (`long_hiar` → `long_hair`). Each links to the same
search with the tag swapped. Excluded tags, wildcards and filters get
none.

## Order and page size

| Filter | Order |
|---|---|
| `order:id` (default), `order:id_asc` | newest / oldest first (`order:created_at` works too) |
| `order:score`, `order:score_asc` | highest / lowest score |
| `order:favcount`, `order:favcount_asc` | most / fewest favourites |
| `order:mpixels`, `order:mpixels_asc` | largest / smallest image |
| `order:filesize`, `order:filesize_asc` | largest / smallest file |
| `order:landscape`, `order:portrait` | widest / tallest first |
| `order:duration`, `order:duration_asc` | longest / shortest video |
| `order:tagcount`, `order:tagcount_asc` | most / fewest tags |
| `order:arttags`, `order:gentags_asc`, … | most / fewest tags in a category |
| `order:comment`, `order:comment_asc` | most / least recently commented (only posts with comments) |
| `order:note`, `order:note_asc` | most / least recently noted (only posts with notes) |
| `order:change`, `order:change_asc` | most / least recently changed, e.g. to follow recent tag edits (`order:updated` works too) |
| `order:rank` | hot posts: from the last two days with a positive score, highest score first, discounted by age (the **Hot** link) |
| `order:upvotes`, `order:downvotes` (and `_asc`) | most / fewest up or down votes |
| `order:comment_bumped`, `order:comment_bumped_asc` | like `order:comment`, leaving out comments posted with **Don't bump the post** |
| `order:comment_count`, `order:note_count` (and `_asc`; `commentcount` and `notecount` work too) | most / fewest comments or notes |
| `order:custom` | in the order of the search's `id:` list: `id:3,1,2 order:custom` |
| `order:md5`, `order:md5_asc` | by the file's MD5, for a stable order that isn't upload order |
| `order:random` | shuffled |
| `ordfav:alice` | alice's favorites, most recently favorited first |
| `ordpool:my_comic` | the pool's posts, in the pool's order |
| `ordfavgroup:best` | the favorite group's posts, in its order |

`limit:100` shows more posts per page (up to the site's maximum, 200 by
default).

## Pages

Results have numbered pages up to page 1000 (configurable). Sorted by id,
"next" links keep working beyond that; they use `page=b<id>` (posts before
that id) and `page=a<id>` (after), which are as fast on page 50,000 as on
page 2.

Counts are exact up to 10,000 posts. Above that, a single tag shows its
known post count, and other searches show "10,000+".

## Searching by image

**Search by image** (`/iqdb_queries`, linked from **Popular** and as
**Look-alikes** under every post) takes a picture, a link to one (a
work's page on a site Moekura reads works too), or a post, and lists the
posts that look most like it, with how alike they are, without uploading
anything. Matches are found by the same perceptual hash as `similar:`,
so a resized or recompressed copy is found, but a crop or an edit may
not be. A video searches by its poster frame; zips, ugoira included,
can't be searched with (search with the ugoira's post instead). Each
search compares the picture with every post, so they're limited to a few
a minute, counted before a link is looked up. A linked picture may be up
to 20 MB (or the upload limit, if that's lower). The API has it as
`POST /api/v1/posts/similar`, and Danbooru clients as
`/iqdb_queries.json`.

## Popular posts and searches

**Popular** in the menu shows what's going on, for a day, a week (the
seven days ending on the date) or a month, with links to earlier and
later ones:

- **Popular**: the best-scored posts posted then.
- **Most viewed**: the posts whose pages were looked at most.
- **Searches**: the tag searches that found posts most often. Searches
  naming a tag the site doesn't have (or with a `*` pattern among the
  tags they exclude or make optional) aren't among them, even when
  their other tags found posts.
- **Missed searches**: tag searches that found nothing, most often
  because of a misspelling or a name the site calls something else;
  an [alias](tags.md) can send them to the right tag.

Each person counts once a day per post or search (visitors without an
account by their address, or their IPv6 `/64`); crawlers and link
previews don't count. Only searches of plain tags are counted (and only
their first page): searches with metatags such as `fav:` or `user:`,
which may name people, are left out. A person adds at most ten missed
searches a day, and each web server counts at most 2,000 different ones
a day. Counts are kept for about a year.

## Feeds

Every search has an Atom feed of its newest posts: the **Feed** link
beside the results, or `/posts.atom?tags=cat+-dog`. Add it to a feed
reader to follow new posts of a tag, an artist (`user:alice` for a
user's uploads), or anything else you can search for. `/comments.atom`
follows the newest comments.

On a private site, feed readers can't log in; make a feed token under
**Settings → Feeds** (which asks for your password, as making an
[API key](../api.md) does) and add `&token=…` to the feed's address.
The token reads feeds as you (with your blacklist) and does nothing
else; making a new one or revoking it stops the old one working, and so
does resetting your password.
