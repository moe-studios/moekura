# The forum

**Forum** in the menu has the site's discussions, in categories:
*General*, *Tags*, *Bugs and features* and *Site news*. Anyone who can
comment can start a topic (**New topic**) or reply, in the same markup as
comments; you can edit your own posts while the topic is open, and vote
other people's posts up or down. Posts and edits go through the same
rate limit and [spam filter](../admin/moderation.md#spam-filter) as
comments.

Topics you haven't read since someone last posted in them are marked
*unread*; opening a topic reads it, and **Mark all read** reads them all.
Stickied topics stay at the top of the list. **Search posts** finds posts
by their words or writer, and the list's search finds topics by title.

## Tag requests

Requesting an alias, an implication or a bulk update starts a topic about
it in *Tags*, with the request and its reason; the request's page links
to the topic, and the topic back to the request. Requests made by those
who can manage tags take effect at once and get no topic.

## Moderation

Staff with *Hide comments and handle reports about them* can, from a
topic's **Moderate** section, stick it to the top, lock it (only staff
can post in a locked topic), delete or restore it, and merge it into
another topic: its posts move there, it's deleted, and links to it lead
to the other. They can also hide posts (hidden posts stay visible to
staff, marked) and edit anyone's. All of this goes in the moderation log.

Danbooru clients can read topics, posts and their own votes, and post, at
`/forum_topics.json`, `/forum_posts.json` and `/forum_post_votes.json`.
