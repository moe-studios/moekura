# Pools

A pool is an ordered collection of posts: a **series** (a comic, a set
of pages meant to be read in order) or a **collection** (posts that
belong together). Find them under **Pools** in the menu.

## Making and changing pools

Anyone with **Create and edit pools** (members, by default) can start a
pool with **New pool** and change any pool's name, kind, description and
posts. The posts are a list of post numbers in order; on the edit page
you can also drag the thumbnails into order. To add one post, open it
and use **Add to a pool** beside it, which puts it at the end.

Every change is kept: **History** shows who added, removed or reordered
posts, and any version can be restored. If someone else saves the pool
while you're editing it, you're told instead of overwriting their
change. Staff who can delete posts can also delete and restore pools.

## Reading

A post in a pool shows a bar above the image with the pool's name, the
post's place in it, and links to the first, previous, next and last
post. Posts opened from a pool's page step through the pool with the
`a` and `d` keys (or the arrow keys).

Series have a reader: **Read** on the pool's page shows the posts one
after another, 20 at a time, and **One page at a time** shows a single
page, with `a` and `d` to turn it (clicking the image turns it too).
Your browser remembers the last page you read, and the pool's page
offers to continue from there.

## Searching

| Filter | Finds |
|---|---|
| `pool:my_comic`, `pool:12` | posts in the pool (by name, regardless of case, or number) |
| `pool:any`, `pool:none` | posts in some pool, or in none |
| `ordpool:my_comic` | the pool's posts, in the pool's order |

Pools also appear in [the API](../api.md) and to
[Danbooru apps](danbooru-clients.md).
