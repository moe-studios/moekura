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

## Deprecated tags

A deprecated tag can't be added to posts any more, but stays on the posts
that already have it until someone takes it off.

## Tag scripts

To tag many posts quickly, open **Tag script** beside search results
(for those who can edit posts), type a script and tick **Apply by
clicking posts**. Clicking a post then applies the script to it instead
of opening it: `tag` adds a tag, `-tag` removes one, and `rating:s` sets
the rating, so `cat_ears -cat rating:g` does all three. Changed posts
are outlined green, refused ones red with the reason below the script.
Each change is in the post's history as if you had edited it.
