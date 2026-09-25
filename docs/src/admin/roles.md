# Roles and permissions

Everyone has a role, and a role is a set of permissions. Logged-out
visitors have the **Anonymous** role. Edit roles under **Admin → Roles**:
rename them, and choose their permissions.

| Permission | Allows |
|---|---|
| View posts | seeing posts, tags and profiles; take it from Anonymous for a [private site](private-sites.md) |
| Upload | uploading posts |
| Upload without approval | skipping the approval queue, when it's on |
| Edit posts and tags | changing a post's tags, rating, source, description and parent; requesting aliases and implications |
| Favorite, Vote | favoriting posts; voting on posts and comments |
| Comment | posting comments, and editing and deleting your own |
| Edit the wiki | |
| Edit notes | adding, moving, changing and deleting notes on posts |
| Create and edit pools | making pools, and changing their posts, names and descriptions (deleting a pool takes *Delete and restore posts*) |
| Flag posts | asking moderators to delete a post |
| Approve posts and handle flags | the approval and flag queues |
| Delete and restore posts | |
| Purge posts | removing deleted posts and their files for good |
| Manage tags, aliases and implications | tag categories, deprecating tags, deciding alias and implication requests |
| See deleted posts | deleted posts and comments |
| Hide comments and handle reports about them | hiding and restoring anyone's comments, and the reported comments queue |
| Ban users and networks | |
| Manage users | changing other users' roles and account status |
| Manage site settings and roles | |
| Read the moderation log | |

The built-in roles and what they start with:

| Role | Rank | Permissions |
|---|---|---|
| Anonymous | 0 | View posts |
| Member | 10 | Anonymous, plus Upload, Edit posts and tags, Comment, Favorite, Vote, Flag posts, Edit the wiki, Create and edit pools, Edit notes |
| Contributor | 20 | Member, plus Upload without approval |
| Janitor | 30 | Contributor, plus Approve posts, Delete and restore posts, Manage tags, See deleted posts, Hide comments |
| Moderator | 40 | Janitor, plus Ban users and networks, Read the moderation log |
| Admin | 50 | everything |

## Upload limits

Each role can limit uploads, under **Admin → Roles** (empty means no
limit):

- **Waiting for approval at once**: when the approval queue is on, how
  many of a user's uploads may wait in it. Members start at 10. Roles
  with *Upload without approval* skip the queue, so this doesn't apply
  to them.
- **Per day**: uploads in the last 24 hours, queued or not.

With **Limits on uploads waiting for approval grow…** ticked in the site
settings, the queue limit follows each user's record, as on Danbooru:
one more for every 10 of their uploads that were approved, one fewer for
every 5 that were deleted, from 1 up to four times the role's limit.

The upload page tells users how many uploads they have left, and why an
upload was refused; the API's `/users/me` says the same under `uploads`.

## Rank

Staff act only on people below them: a moderator can ban members and
janitors, but not other moderators or admins. The same goes for changing
roles: you can give or take away only roles ranked below your own, and
never change your own role or status. That keeps a mistake, or a
compromised account, from locking out the people above it.

From the shell, `moekura admin set-role NAME ROLE` changes anyone's role,
which is how you recover if the last admin loses access.
