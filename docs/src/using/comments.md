# Comments

Every post has comments under the image. Anyone with the **Comment**
permission (members, by default) can write one; they use the same
[formatting as the wiki](wiki.md#formatting), including `[quote]` blocks
and `comment #45` links. **Reply** starts a new comment quoting the one
you're answering. The newest 50 comments are shown on the post; the rest
are a link away.

You can edit and delete your own comments; edited ones say so. Deleting
a post's last comment takes it out of `order:comment`. Deleted posts
can't be commented on, and comments are rate limited: a few at once,
then one every 20 seconds. Edits count toward the same limit.

On public sites, a comment that looks like spam (links from a brand-new
account, the same text over and over, or words the staff listed) is
held until the staff check it; the same goes for forum posts and
messages, and for edits, which hide a comment again until it's checked.
See [the spam filter](../admin/moderation.md#spam-filter).

Tick **Don't bump the post** to comment without moving the post up in
`order:comment_bumped`; it still counts for `order:comment`. Moderators
can **Pin to top** a comment, which then comes first among the post's
comments.

**Comments** in the menu lists the newest comments on the whole site,
beside their posts; each profile links to that user's comments.

## Votes and reports

Vote comments up or down with the arrows (not your own). Comments voted
down to −5 or lower are folded away, a click from being read.

**Report** asks the moderators to look at a comment; they can hide it,
restore it later, or dismiss the report. See
[Moderation](../admin/moderation.md#comments).

## Searching

| Filter | Finds |
|---|---|
| `commentcount:>0` | posts with comments |
| `order:comment` | posts with comments, most recently commented first |
