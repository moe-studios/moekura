# Moderation

Everything here is under **Moderation** in the menu, for people whose role
allows it, and every action is recorded in the moderation log with who did
it and why.

## The approval queue

When **New uploads wait for approval** is ticked (**Admin → Settings**), uploads by people without *Upload without approval* are
*pending*: only their uploader and staff see them. Approve them, or reject
them with a reason, from **Moderation → Approval queue**, or under
**Moderate** on the post's own page. The queue leaves out your own
uploads; find those with `status:pending user:yourname` and approve them
from their page.

An approver can also *disapprove* a post: pass on it without rejecting
it, saying whether it breaks the rules, is of poor quality, or just isn't
for them, with an optional note. The post stays pending and leaves that
approver's queue, while other approvers see the disapprovals (and the post
page lists them). The queue holds the posts found by `status:unmoderated`:
pending posts the approver didn't upload and hasn't disapproved. A user's
moderation record counts the posts they disapproved.

The queue has a search box, which takes the usual
[search syntax](../using/search.md) (tags, `user:name`, `rating:e` and so
on, but not `status:`), and a choice of order: oldest first (the
default), newest first, score, favorites, fewest tags or size. Tick posts
to approve or reject them together (up to 100 at once, with one reason
for the rejections); any that someone else dealt with meanwhile are
skipped.

## Flags

Members flag posts that should go, with a reason. Flagged posts stay
visible, marked *flagged*, and appear under **Moderation → Flags**.
Dismiss the flags to keep the post, or delete it, which upholds them.
Each person can flag posts and report comments about ten times at once,
then once a minute.

## Comments

Members report comments, with a reason; reported comments appear under
**Moderation → Reported comments**. Staff with *Hide comments* can hide
any comment (which upholds its reports) and restore it later, or dismiss
the reports. Hidden comments, and those their authors deleted, stay
visible to staff, marked *deleted*. Comments voted down to −5 or lower
are collapsed for everyone.

## Reasons

Deleting, rejecting and flagging a post offer the site's preset reasons
(*Duplicate*, *Poor quality*, *Off-topic*, *Breaks the rules* to begin
with), so reasons stay consistent, with a box for details or a reason of
one's own (*Other*). A preset with details is recorded as
"Poor quality: blurry". Change the lists under **Admin → Settings →
Moderation reasons**, one per line; empty lists leave just the box. The
API and Danbooru clients send free text as before.

## Deleting, restoring, purging

Deleting a post (with a reason, which is required and shown on the post) hides it from everyone
without *See deleted posts*, except its uploader, who still sees the post
and why it went, but can't change it. It can be restored. Purging a deleted post
removes it, its files and its history for good, in the background.

### Purging in bulk

Staff with *Purge posts* (admins, by default) purge many deleted posts at
once under **Moderation → Purge**. Search the deleted posts with the
usual [search syntax](../using/search.md), such as `user:name` for a
spam account's leftovers (`status:deleted` is implied, and other
statuses are refused), or leave the search empty for every deleted post.
The preview says how many posts match and shows the first of them; tick
some and **Purge ticked** (up to 500 at once), or **Purge all** to purge
every match. Either way, tick the box confirming the posts, their files
and their history go for good.

A background job then purges the posts one by one, just as purging each
would, logging each purge as yours; starting the purge is logged too,
with its search or the posts ticked. Posts are checked again as the job
gets to them, so one restored meanwhile is skipped. A post whose files
can't be removed is counted as failed and left deleted (purge it again
later); if several in a row fail, the job stops and is retried later,
carrying on where it stopped rather than starting over. The page lists
recent purges with how many posts were purged, skipped and failed. The
API has the same operation (`POST /api/v1/moderation/purge` with a
`query` or `post_ids`, followed with
`GET /api/v1/moderation/post-batches/{id}`).

## Locks

Staff with *Lock posts* (moderators, by default) lock a post's rating,
tags, notes or status under **Moderate** on the post page, for instance
to end an edit war. The post says what's locked, and locking and
unlocking show in its history and the log. For everyone without *Lock
posts*:

- a locked **rating** or **tags** can't be changed: not by editing the
  post (on the site, through the API, Danbooru apps or tag scripts), nor
  by reverting to an earlier version;
- locked **notes** can't be added, changed or deleted;
- a locked **status** means the post can't be flagged, approved,
  rejected, deleted, restored or appealed, nor its flags dismissed.

Mass tag edits leave posts with locked tags alone, and the tagger doesn't
touch locked tags or ratings. Tag aliases and implications still apply to
every post, so a renamed tag stays renamed.

## Appeals

The uploader of a deleted post (and anyone who can see deleted posts),
if their role can flag posts, can *appeal* it from the post page with a
reason. A post has one open appeal at a time, and each person can appeal
three posts at once, then one more every four hours. Open appeals are
listed under **Moderation → Appeals** (and found with `status:appealed`)
for those who can delete and restore posts: **Restore** brings the post
back and grants the appeal (so does restoring it any other way); **Keep
deleted** turns the appeal down, with an optional reason, in the log. The
post page keeps its appeals and how they ended, for staff and the
uploader.

## Bans

Ban a user from their profile, for a set time or until lifted, with a
reason. Banned users can still log in and look around as visitors do, see
why they're banned, and can't change anything; their API keys are limited
the same way. Banning someone who's already banned replaces their ban
with the new reason and length. Timed bans last up to 3650 days.

Networks (an address or a CIDR range such as `203.0.113.0/24`, or
`2001:db8::/64` for IPv6, where one household usually has a whole `/64`)
are banned under **Moderation → Bans**, partly or fully:

- a **partial** ban lets requests from the network read, but not
  register, log in or change anything;
- a **full** ban keeps the network from seeing the site at all: every
  page and API call answers that the network is banned, with the reason.

The range may not include your own address, or be wider than a `/8`
(IPv4) or `/16` (IPv6), and only staff who manage users can fully ban a
range wider than a `/24` (IPv4) or `/48` (IPv6). As with banning users,
rank counts: you can't ban a range, or lift a ban on one, that includes
an address of anyone ranked at or above you (yourself included), as
kept under [Addresses](#addresses) or by a session still open. If a
network ban locks staff out anyway, lift it from the shell with
`moekura admin lift-network-ban ADDRESS`, which lifts every ban covering
that address (or overlapping a range) and logs it.

IPv4 clients are always matched (and shown) as IPv4, also when the
server listens on IPv6 as well (`[::]`). An address or range copied from
a log that writes them IPv4-mapped (`::ffff:203.0.113.7`,
`::ffff:203.0.113.0/120`) is banned as the IPv4 it stands for. Network
bans are kept in memory on every node, so checking them costs nothing
per request; changes reach other nodes within moments.

## A user's record

Staff who can ban users or read the log see a **Moderation record** link
on each profile (and the log's user names lead there too). The page puts a
user's history in one place: their role, status, when they joined and were
last seen, whether two-factor login is on and whether they're kept from
automatic promotion; their bans, with controls to ban or lift the ban;
their uploads by status and the recent deletions with reasons; the flags
on their uploads, and the flags they filed with how many were upheld or
dismissed; reports about their comments and their hidden comments; and,
for those who read the log, what was logged about them and what they
did themselves.

### Deleting all of a user's uploads

To clean up after a spam account, staff with *Delete posts* can delete
every upload of a user ranked below them at once: **Delete all uploads**
under *Uploads* on the record says how many posts that is, and asks for a
reason (the same presets as deleting one post) and a tick to confirm.
A background job then deletes the user's active, flagged and pending
posts in batches, just as deleting each one would: the posts show the
reason, open flags on them are upheld, tag counts drop, each deletion is
logged as yours, and every post can still be restored (webhooks aren't
sent a `post.deleted` event for each, though). Posts already
deleted are left out; those whose status is locked are skipped unless
you have *Lock posts*. The record shows the progress: how many posts
were deleted, skipped (dealt with meanwhile, or locked) and failed. Only
one such deletion runs per user at a time, and the account itself stays;
ban it separately. The API has the same operation
(`POST /api/v1/users/{name}/delete-uploads`, followed with
`GET /api/v1/moderation/post-batches/{id}`).

### Staff notes

The same staff keep private notes about users, on the profile and the
record: who wrote each and when, in the same markup as comments. Only
staff see them, not the user. Authors delete their own notes; those who
can ban users delete anyone's.

### Addresses

For staff who can ban users, on the record of someone ranked below them,
the record also lists the addresses the account used, when each was
first and last seen, and the other accounts (also ranked below them)
seen on the same addresses, which is how ban evaders usually show. Each
address has shortcuts to ban it, or its `/24` (IPv4) or `/64` (IPv6)
network, under **Moderation → Bans**.

What's stored, for your privacy policy: for each account, each address it
logged in or changed something from (posting, editing, voting, changing
settings and so on; merely reading pages isn't recorded), with the first
and last time it was seen, at most hourly. Addresses are kept for 365 days
after they were last seen, then forgotten by a daily job; change that
under **Admin → Settings** (**Keep the addresses accounts use**), where 0
keeps none and stops recording them. Deleting an account deletes its
addresses. Sessions separately keep the address they were started from
until they end. The addresses also help at login: while someone guesses
an account's password from many networks at once, its owner can still
log in from a network (an IPv4 address or an IPv6 `/64`) it used, or
from elsewhere with a captcha, if one is set up.

## Renaming users

Staff who can ban users can rename anyone ranked below them with
**Rename** on their profile, say for an offensive or impersonating name,
with a reason for the log. Unlike users' own changes, it has no waiting
time. Their profile lists their former names, and the old name still
finds them (see [Your name](../using/account.md#your-name)).

## Spam accounts

Besides rate limits, email confirmation and approval of new accounts
(**Admin → Settings → Registration**), two settings under **Admin →
Settings → Spam** keep spam accounts out:

- **Email domains**: a list of domains whose addresses are refused (such
  as disposable-mail services), or the only ones accepted (such as a
  school's). Each domain covers its subdomains. It applies when signing up
  and changing an address; accounts made through single sign-on don't
  take a refused address, and where new accounts confirm their address,
  can't be made without one the site accepts.
- **Captcha**: with a service set up (see
  [`[auth.captcha]`](../configuration.md#authcaptcha)), ask for it when
  signing up, and on comments by accounts younger than a number of days.
  It isn't asked for when signing up through single sign-on, where the
  provider has checked who is logging in; the limit of new accounts per
  address still applies.
  Logging in to an account asks for it, whatever these say, while
  someone guesses its password from many networks at once.

## Spam filter

Comments, forum posts and messages that look like spam are held for
review instead of appearing: nobody but their writer knows they exist
(and they're told only that the staff will check it). By default the
filter runs unless the site is [private](private-sites.md); **Admin →
Settings → Spam** can turn it on or off for good, and choose what it
holds:

- links (`http://`, `https://`, `www.`) from accounts younger than 3
  days;
- text its writer already posted 3 times in the past day, anywhere;
- anything containing one of the **spam words**, one word or phrase per
  line, whatever the case.

Set a number to 0 to stop holding for it. Changing a comment or forum
post is checked the same way: a change that would be held as a new text
hides it again until the staff approve it. People who can *Hide comments
and handle reports about them* are never held, and review what is under
**Moderation → Held for review**, oldest first, with why each was held.
**Approve** lets it through as if just posted (mentions, replies and
messages notify then, and webhooks fire); **Reject** keeps it hidden for
good, and a rejected message never reaches its recipient. Both go in the
moderation log. Hiding or restoring a held comment or forum post the
usual way also settles it.

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
bring the tags they imply. The tags to add can also take `-tag` and
`rating:e` to set every post's rating; locked tags and ratings are left
alone. Deleted posts are only changed if the search asks for them
(`status:deleted` or `status:any`).

Bulk update requests (**Tags → Requests**) bundle several alias,
implication, category and mass edit changes; approving one applies them
in order, and approving or rejecting it is logged. One with mass edit
(`update`) lines needs *Mass edit tags* to approve, from someone other
than its requester, and each of its mass edits is logged as the
approver's. See
[Tags](../using/tags.md#bulk-update-requests).

## Post changes and undoing vandalism

**Moderation → Post changes** (`/post_versions`, also linked from each
post's history and each profile) lists every change to posts across the
site, newest first, for anyone: tags added and removed, rating, source,
parent, description and locks. Filter it by who made the change, the
post, a tag added or removed, and a range of days. Changes to tags,
wiki pages, pools and notes have lists of their own (`/tag_versions`,
`/wiki_page_versions`, `/pool_versions`, `/note_versions`), filtered by
user the same way and linked from each user's moderation page.

Filtered to one user, it offers those with *Undo a user's post edits*
(moderators, by default) **Undo their edits**, for users ranked below them: in the
background, every post edit the user made in the range is taken back.
Tags they added come off and tags they removed go back; a rating,
source, description or parent they set is put back where nobody changed
it since, and locked tags and ratings are left alone. Uploads aren't
edits and stay. Each post's history credits whoever started the undo,
and the log records it.

## Stats and reports

`/stats` (linked from every page's footer) shows anyone the site's
totals: posts, tags in use, users, favorites, comments, forum posts, wiki
pages, pools, artists and notes.

Staff who can read the moderation log also get **Reports** there,
charting uploads, post changes, approvals, comments, forum posts, wiki
edits, note changes, favorites, post votes and new accounts over the last
30, 90 or 365 days, with the most active users for each. Picking a user
charts theirs alone.

The `stats.refresh` [job](jobs.md) counts all of this hourly (the first
time, the past year), so the pages cost nothing to view. Days are UTC.

## The moderation log

**Moderation → Log** lists every staff action: approvals, deletions,
purges, flag decisions, bans, tag and relation changes, role and setting
changes (including those made from the shell). Filter it by action,
moderator, post, the user acted on, or a range of days. Role, status and
setting changes show what they changed from.
