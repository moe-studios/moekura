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
Each person can flag posts and report comments about ten times at once,
then once a minute.

## Comments

Members report comments, with a reason; reported comments appear under
**Moderation → Reported comments**. Staff with *Hide comments* can hide
any comment (which upholds its reports) and restore it later, or dismiss
the reports. Hidden comments, and those their authors deleted, stay
visible to staff, marked *deleted*. Comments voted down to −5 or lower
are collapsed for everyone.

## Deleting, restoring, purging

Deleting a post (with a reason, which is required and shown on the post) hides it from everyone
without *See deleted posts*. It can be restored. Purging a deleted post
removes it, its files and its history for good, in the background.

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
(IPv4) or `/16` (IPv6). Network bans are kept in memory on every node,
so checking them costs nothing per request; changes reach other nodes
within moments.

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

### Staff notes

The same staff keep private notes about users, on the profile and the
record: who wrote each and when, in the same markup as comments. Only
staff see them, not the user. Authors delete their own notes; those who
can ban users delete anyone's.

### Addresses

For staff who can ban users, the record also lists the addresses the
account used, when each was first and last seen, and the other accounts
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
until they end.

## Spam accounts

Besides rate limits, email confirmation and approval of new accounts
(**Admin → Settings → Registration**), two settings under **Admin →
Settings → Spam** keep spam accounts out:

- **Email domains**: a list of domains whose addresses are refused (such
  as disposable-mail services), or the only ones accepted (such as a
  school's). Each domain covers its subdomains. It applies when signing up
  and changing an address; accounts made through single sign-on simply
  don't take a refused address.
- **Captcha**: with a service set up (see
  [`[auth.captcha]`](../configuration.md#authcaptcha)), ask for it when
  signing up, and on comments by accounts younger than a number of days.

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

Bulk update requests (**Tags → Requests**) bundle several alias,
implication, category and mass edit changes; approving one applies them
in order, and approving or rejecting it is logged. See
[Tags](../using/tags.md#bulk-update-requests).

## The moderation log

**Moderation → Log** lists every staff action: approvals, deletions,
purges, flag decisions, bans, tag and relation changes, role and setting
changes (including those made from the shell). Filter it by action,
moderator, post, the user acted on, or a range of days. Role, status and
setting changes show what they changed from.
