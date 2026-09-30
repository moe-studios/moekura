# Tags

Tags describe what's in a post. They're lower case, with underscores
instead of spaces (`long_hair`), and belong to a category:

| Category | For |
|---|---|
| general | what's in the picture |
| artist | who made it |
| copyright | the series or franchise |
| character | who's in it |
| meta | things about the file, such as `animated` or `translated` |

When you tag a post, write a new tag with a prefix to put it in a category:
`artist:someone`. A tag already on posts keeps its category, unless you
can manage tags; then the prefix moves it, as does editing it in the tag
list.

Each tag can have a [wiki page](wiki.md) describing it.

## Metatags in the tag box

The tags box on the edit and upload forms takes more than tags, as on
Danbooru. `-tag` takes a tag off, and these metatags change other
things:

| Metatag | Does |
|---|---|
| `rating:g`, `s`, `q`, `e` (or the full name) | sets the rating |
| `source:https://…`, `source:none` | sets or clears the source |
| `parent:123`, `parent:none` or `-parent` | sets or clears the parent; `-parent:123` clears it only if it's 123 |
| `child:123`, `-child:123` | makes post 123 a child of this one, or stops it being one |
| `pool:12`, `pool:name`, `-pool:12` | adds the post to the end of a pool, or takes it out |
| `newpool:name` | starts a pool with the post (or adds it to the pool of that name) |
| `fav`, `-fav` | favorites the post, or stops favoriting it |
| `favgroup:12`, `favgroup:name`, `-favgroup:12` | adds the post to one of your favorite groups, or takes it out |
| `upvote`, `downvote` | votes on the post |

Each needs the permission it would need done by hand (editing pools to
use `pool:`, favoriting to use `fav`, …), and is recorded where that
would be: parents in the posts' history, pools in the pool's. A metatag
you can't use, or one naming something that doesn't exist, stops the
save with the reason. Metatags in the box win over the form's own
rating, source and parent fields. They also work in tag scripts and the
APIs' tag fields; in a mass edit, only `-tag` and `rating:` do.

## Warnings after saving

After an upload or an edit, the post page lists what may be missing,
without stopping the save: no artist, copyright or character tag, fewer
than 10 general tags, tags no other post has yet (often a typo), and a
category prefix that couldn't move an existing tag (`artist:cat` when
`cat` is already a general tag; only those who manage tags can move
used tags).

Sites can also have **request tags** added by themselves (**Admin →
Settings**, off by default): `artist_request` while a post has no artist
tag and `tagme` while it has fewer than 10 general tags. They come off
again when an edit fixes that.

## Copying tags from related posts

A post's **Edit** form lists its parent and children under **Copy
tags**. Click one to add that post's tags to the tags box, then change
what doesn't fit and save (without scripts, the click adds them and
saves at once).

## Suggestions from the tagger

On sites that run the [tagger](../admin/tagger.md), a model looks at each
new upload, usually within a minute, and suggests tags and a rating. They
show under **Edit** on the post, most confident first, with how sure the
model is: click one to add it to the tags box (or, without scripts, to add
it and save), then save. Tags the post already has aren't suggested, nor
tags below the confidence the site asks for in their category.

The model is often right about what's in a picture and sometimes
confidently wrong, so check before saving. Search for `ai:tag` to find
posts where a tag is suggested but not yet applied, for tidying up many
posts at once.

Sites can also have the tagger apply the suggestions it is surest of by
itself. Those edits appear in the post's history as the tagger's account
(normally `tagger`), and are undone like anyone's.

## Aliases and implications

An **alias** makes one tag stand for another: with `kitty` aliased to
`cat`, posts tagged `kitty` are tagged `cat` instead, and searching for
`kitty` finds them.

An **implication** adds a tag: with `cat` implying `animal`, every post
tagged `cat` is also tagged `animal`.

Anyone who can edit posts can request them under **Tags → Aliases** and
**Implications**; people who can manage tags approve them. Once approved,
they're applied to existing posts in the background, and to every edit
after. Each post's history shows which changes came from an alias or
implication.

### Voting and discussion

Each request has its own page (click its status or score in the list):
members vote for or against it while it's pending, and discuss it
underneath. Votes help staff decide; they don't decide by themselves.

## Bulk update requests

**Tags → Requests** holds requests for several changes at once, decided
together. A request has a title, a reason, and a script with one change
a line:

| Line | Does |
|---|---|
| `alias kitty -> cat` | aliases `kitty` to `cat` |
| `imply cat -> animal` | makes `cat` imply `animal` |
| `unalias kitty -> cat`, `unimply cat -> animal` | ends an alias or implication |
| `update cat_ears solo -> animal_ears -cat_ears` | a mass edit: the posts a search finds get the tags after the arrow, and lose those with `-` |
| `category someone -> artist` | moves a tag to a category |

Lines starting with `#` are ignored. Mistakes are pointed out, by line,
when you send the request. Members vote and discuss as for single
requests; staff who manage tags approve (which applies the lines in
order, in the background) or reject it, and you can withdraw your own
while it's pending. If a line can't be applied (say, it would make an
implication loop), the request stops there, marked failed with the
reason; the lines before it stay applied.

## History

**Tags → History** lists every change to tags: when each was created and
who changed its category or deprecation, newest first, with the old and
new values. Filter it by tag or by user; a tag's edit page links to its
own history.

## Deprecated tags

A deprecated tag can't be added to posts any more, but stays on the posts
that already have it until someone takes it off.

## Tag scripts

To tag many posts quickly, open **Tag script** beside search results
(for those who can edit posts), type a script and tick **Apply by
clicking posts**. Clicking a post then applies the script to it instead
of opening it: `tag` adds a tag, `-tag` removes one, and `rating:s` sets
the rating, so `cat_ears -cat rating:g` does all three; the other
[metatags](#metatags-in-the-tag-box) (`pool:12`, `fav`, …) work too. Changed posts
are outlined green, refused ones red with the reason below the script.
Each change is in the post's history as if you had edited it.
