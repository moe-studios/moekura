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
| Favorite, Vote | |
| Flag posts | asking moderators to delete a post |
| Approve posts and handle flags | the approval and flag queues |
| Delete and restore posts | |
| Purge posts | removing deleted posts and their files for good |
| Manage tags, aliases and implications | tag categories, deprecating tags, deciding alias and implication requests |
| See deleted posts | |
| Ban users and networks | |
| Manage users | changing other users' roles and account status |
| Manage site settings and roles | |
| Read the moderation log | |

The built-in roles and what they start with:

| Role | Rank | Permissions |
|---|---|---|
| Anonymous | 0 | View posts |
| Member | 10 | Anonymous, plus Upload, Edit posts and tags, Favorite, Vote, Flag posts |
| Contributor | 20 | Member, plus Upload without approval |
| Janitor | 30 | Contributor, plus Approve posts, Delete and restore posts, Manage tags, See deleted posts |
| Moderator | 40 | Janitor, plus Ban users and networks, Read the moderation log |
| Admin | 50 | everything |

## Rank

Staff act only on people below them: a moderator can ban members and
janitors, but not other moderators or admins. The same goes for changing
roles: you can give or take away only roles ranked below your own, and
never change your own role or status. That keeps a mistake, or a
compromised account, from locking out the people above it.

From the shell, `uwubooru admin set-role NAME ROLE` changes anyone's role,
which is how you recover if the last admin loses access.
