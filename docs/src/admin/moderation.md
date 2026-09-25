# Moderation

Everything here is under **Moderation** in the menu, for people whose role
allows it, and every action is recorded in the moderation log with who did
it and why.

## The approval queue

When **New uploads wait for approval** is ticked (**Admin → Settings**), uploads by people without *Upload without approval* are
*pending*: only their uploader and staff see them. Approve them, or reject
them with a reason, from **Moderation → Approval queue**.

## Flags

Members flag posts that should go, with a reason. Flagged posts stay
visible, marked *flagged*, and appear under **Moderation → Flags**.
Dismiss the flags to keep the post, or delete it, which upholds them.

## Comments

Members report comments, with a reason; reported comments appear under
**Moderation → Reported comments**. Staff with *Hide comments* can hide
any comment (which upholds its reports) and restore it later, or dismiss
the reports. Hidden comments, and those their authors deleted, stay
visible to staff, marked *deleted*. Comments voted down to −5 or lower
are collapsed for everyone.

## Deleting, restoring, purging

Deleting a post (with a reason, shown on the post) hides it from everyone
without *See deleted posts*. It can be restored. Purging a deleted post
removes it, its files and its history for good, in the background.

## Bans

Ban a user from their profile, for a set time or until lifted, with a
reason. Banned users can still log in and look around as visitors do, see
why they're banned, and can't change anything; their API keys are limited
the same way.

Networks (an address or a CIDR range such as `203.0.113.0/24`) are banned
under **Moderation → Bans**. Requests from them can read, but not
register, log in or change anything. The range may not include your own
address, or be wider than a `/8` (IPv4) or `/16` (IPv6).

## Tag aliases and implications

Members request them under **Tags → Aliases** or **Implications**; people
who can manage tags approve or reject them (their own requests apply at
once). See [Tags](../using/tags.md).

## Mass tag edits

**Moderation → Mass edit** (for those with *Mass edit tags*) adds and
removes tags on every post a search finds: `cat_ears` → add
`animal_ears`, remove `cat_ears`. **Preview** shows how many posts match
and the first of them; **Change** starts a background job. The page
lists recent mass edits with their progress. Changes show in each
post's history, credited to whoever started the edit, and added tags
bring the tags they imply. Deleted posts are only changed if the search
asks for them (`status:deleted` or `status:any`).

## The moderation log

**Moderation → Log** lists every staff action: approvals, deletions,
purges, flag decisions, bans, tag and relation changes, role and setting
changes (including those made from the shell). Filter it by action,
moderator or post.
