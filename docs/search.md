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

## Filters

Filters look like `name:value`. Put `-` in front of one to exclude what it
matches (`-rating:e`); `order:` and `limit:` can't be excluded.

| Filter | Example | Meaning |
|---|---|---|
| `rating:` | `rating:e,q` | rating `g`eneral, `s`ensitive, `q`uestionable or `e`xplicit (letters or names, comma-separated) |
| `score:` | `score:>=10` | score |
| `favcount:` | `favcount:>5` | number of favourites |
| `id:` | `id:1000..2000` | post number |
| `user:` | `user:alice` | uploaded by this user |
| `fav:` | `fav:alice` | favorited by this user |
| `width:`, `height:` | `width:>=1920` | size in pixels |
| `mpixels:` | `mpixels:>2` | megapixels (width × height ÷ 1,000,000) |
| `ratio:` | `ratio:16:9`, `ratio:<1` | width ÷ height (`16:9` or a number; exact values match within 0.01) |
| `filesize:` | `filesize:>2mb` | file size, in bytes or with `kb`, `mb`, `gb` (an exact size with a unit matches within 5%) |
| `duration:` | `duration:>30` | length of a video, in seconds |
| `filetype:` | `filetype:png,webm` | file type: `jpg`, `png`, `gif`, `webp`, `avif`, `jxl`, `mp4`, `webm` |
| `date:` | `date:2026-01` | upload date (UTC): a day, month or year |
| `md5:` | `md5:d41d8cd9…` | the file's MD5 hash |
| `parent:` | `parent:123`, `parent:none`, `parent:any` | a post and its children, posts without a parent, or posts with one |
| `tagcount:` | `tagcount:<5` | number of tags |
| `status:` | `status:deleted` | `pending`, `active`, `flagged`, `deleted` or `any` (see below) |

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

### Statuses

Searches show active and flagged posts, plus your own uploads that are
waiting for approval. Staff who review uploads also see pending posts.
Deleted posts only appear with `status:deleted` or `status:any`, and only to
those allowed to see them.

## Order and page size

| Filter | Order |
|---|---|
| `order:id` (default), `order:id_asc` | newest / oldest first |
| `order:score`, `order:score_asc` | highest / lowest score |
| `order:favcount`, `order:favcount_asc` | most / fewest favourites |
| `order:mpixels`, `order:mpixels_asc` | largest / smallest image |
| `order:filesize`, `order:filesize_asc` | largest / smallest file |
| `order:landscape`, `order:portrait` | widest / tallest first |
| `order:duration`, `order:duration_asc` | longest / shortest video |
| `order:tagcount`, `order:tagcount_asc` | most / fewest tags |
| `order:random` | shuffled |
| `ordfav:alice` | alice's favorites, most recently favorited first |

`limit:100` shows more posts per page (up to the site's maximum, 200 by
default).

## Pages

Results have numbered pages up to page 1000 (configurable). Sorted by id,
"next" links keep working beyond that; they use `page=b<id>` (posts before
that id) and `page=a<id>` (after), which are as fast on page 50,000 as on
page 2.

Counts are exact up to 10,000 posts. Above that, a single tag shows its
known post count, and other searches show "10,000+".
