# Your account

## Settings

**Settings**, at the top of every page, holds how the site looks to you:

- **Posts per page**, the **theme** and light or dark **mode**.
- **Blacklist**: posts matching a line are left out of grids, with a
  count and a link to show them. Searches, popular posts, an artist's
  posts and your feeds leave them out as part of the search, so every
  page is full and the counts don't include them. Other grids (pools,
  favorite groups, comments, uploads) leave them out of what they show.
  Tick **Blur blacklisted posts** to keep them in grids, blurred,
  instead. The API leaves blacklists to clients.
- **Safe mode** shows only general-rated posts, everywhere: searches,
  post pages, pools and the API.
- Post pages show large images resized, saying so above them with a link
  to the original. **Show original images** always shows the original.
- **Include deleted posts in searches**, for those allowed to see them.
- **Large thumbnails** use the site's second thumbnail size in grids.
- **Hide comments** leaves comments off post pages, with a link to read
  them.
- **Suggest tags while typing** and **Keyboard shortcuts** can be turned
  off.
- **Time zone** is used for the dates pages show. Otherwise they're in
  UTC.
- **Language**, on sites with more than one, sets the language pages
  are shown in; otherwise your browser's choice is followed.
- **Custom CSS** is applied after the site's styles, for you alone.
- **Email me my notifications** (on sites that send mail, and once your
  address is confirmed) sends each [notification](#notifications) to
  your email as well.

The rest of this page is under **Settings → Your email address and
password**.

## Profiles

Everyone's profile, at `/users/name`, opens with their banner, profile
picture, name, role and bio, and a few headline counts; below are their
latest uploads and favorites (leaving out what your blacklist hides),
each with a link to all of them.

To change yours, choose **Edit profile** on it, or **Settings → Your
profile picture, banner and bio**:

- The **profile picture** is cut to a square, and the **banner** to 3:1,
  keeping the part of the picture that looks most interesting. They're
  stored at most 400 × 400 and 1500 × 500 pixels, without the file's
  metadata. JPEG, PNG, GIF, WebP and AVIF pictures of up to 10 MB work;
  an animation keeps its first frame.
- The **bio**, of up to 4000 characters, is formatted as comments are.

While banned you can't change them. Staff who can rename someone can
also clear their picture, banner or bio from their profile, under
**Clear profile**; the moderation log keeps the bio removed.

The details beside the latest posts count what they've done, each
count linking to the list or search behind it: uploads (and those since
deleted), the score their uploads add up to, favorites and favorite
groups, post, note, wiki and pool changes, comments, forum posts and
[feedback](#feedback). Approvers' profiles count the posts they
approved; you and staff who approve posts also see your up- and
downvotes, which are private otherwise.

Below them, a chart shows uploads in each of the last 12 months, and a
list the tags used most on their latest 1000 uploads (leaving out meta
tags), each linking to their uploads with that tag.

## Notifications

**Notifications**, at the top of every page, lists what happened that
concerns you, with a count of those you haven't read. You're notified
when someone:

- mentions you as `@name` in a comment, a forum post or a request's
  discussion, or quotes you (a `[quote]` starting `name said:`, as
  **Reply** writes);
- sends you a [message](messages.md);
- posts in a [forum](forum.md) topic you've posted in;
- approves or rejects a request you made or voted on;
- leaves [feedback](#feedback) on your account.

Opening one marks it read and goes to what it's about; **Mark all read**
clears the count. Nobody you've blocked from messaging you can notify
you either. Read notifications are forgotten after 90 days.

## Feedback

Staff and senior users (with *Leave feedback on users of lower rank*;
contributors and up, by default) can leave **positive**, **neutral** or
**negative** feedback on someone ranked below them, with **Leave
feedback** on their profile. Profiles count it, linking to the whole
list at `/user_feedbacks?user=name`; anyone can read it, and the person
it's about is notified. Its writer can edit it. Staff who can ban users
can delete and restore it, which goes in the moderation log. Negative
feedback can keep a member from [automatic
promotion](../admin/roles.md#automatic-promotion).

## Your name

**Settings → Your name** changes the name you're known and log in by,
once every 7 days. Your profile lists your former names, and links to
your old profile and searches such as `user:oldname` and
`ordfav:oldname` keep finding you, unless someone else takes the name
later. Staff who can ban users can rename anyone ranked below them,
with **Rename** on their profile, at any time; that goes in the
moderation log.

A name may not end in `.json`: `/users/name.json` is an address of the
[Danbooru API](danbooru-clients.md), so the profile couldn't be opened.

## Email address and password

Changing either needs your current password. Changing the password logs
you out everywhere else, and reset links already emailed stop working.
**Also revoke my API keys and feed token**, ticked unless you untick it,
revokes those too: do that if you think someone else got in.

An address is written plainly, as `name@example.com`, without a name
in front or angle brackets. Mail isn't sent to one saved in another
form before that was required: it shows as not confirmed until you
replace it with a plain one.

On sites that send mail, a new address only replaces the old one once
you follow the link sent to it; so does the address given when signing
up, unless the site has new accounts confirm theirs before they can log
in. Asking for an address another account already has gets the same
answer as any other, and that account is emailed about it instead, so
nobody can use these forms to find out who has an account here. On
sites without mail, the address changes straight away, so a taken one is
refused.

**Forgot your password?** on the login page emails you a link to choose
a new one. The link works for an hour, and only while that's still your
address. Using it logs you out everywhere, revokes your API keys and
feed token, and unlinks any [single sign-on](#single-sign-on) account
linked since you signed up, in case someone else had got in (link yours
again afterwards). An account made through single sign-on keeps the one
it was made with.

Logging in is limited, against password guessing: five tries at an
account from one network (an IPv4 address, or an IPv6 `/64`), then one
every 30 seconds; and ten at an account from all networks together,
then one every 30 seconds. Someone guessing your password from their
network doesn't keep you from logging in from yours. While people guess
it from many networks at once, you can still log in from a network you
used the account from before (if the site keeps the addresses accounts
use, as it does by default), and from anywhere by also solving a
captcha (on sites with a captcha service).

## Single sign-on

On sites set up for it, **Log in with …** on the login page logs you in
through another service's account. The first time, that makes you an
account here (if the site is taking new ones).

To use it with an account you already have, log in with your password
and choose **Link** under **Single sign-on**, confirming your password;
finish at the provider in the same browser. (It can't be done with an API
key.) If your address is confirmed, you're emailed when an account is
linked. You can unlink it later, as long as you have a password to log in
with instead. Accounts made through single sign-on have no password; to
set one, use **Forgot your password?** if the site sends mail. They need
one to link another account at the provider, too.

## Two-factor login

With two-factor login on, logging in needs a code from an authenticator
app (such as Aegis, 2FAS, Google Authenticator or 1Password) as well as
your password, so a leaked password isn't enough to get in.

1. Open **Two-factor login settings** and choose **Set it up**.
2. Scan the QR code with your app, or type in the key shown under it.
3. Enter the code the app shows, to check it has the key.
4. Save the ten **recovery codes** somewhere safe: each logs you in once
   if you lose your device. You can make new ones at any time, which
   replaces the old ones.

When logging in, enter a code from the app, or a recovery code, after
your password (or after single sign-on). If your device's clock is off by more than about half a
minute, codes won't work; most phones set the time automatically.

Wrong codes are counted until a right one, even across logins, since
whoever types them already got past your password:

- After 5 wrong codes in a row, you're emailed about it (on sites that
  send mail, if your address is confirmed). If it wasn't you, change
  your password.
- After 10, codes from the app stop working for 15 minutes, and each
  further wrong code doubles that, up to a day. Recovery codes still
  work meanwhile, and logging in with one lifts the wait.

If you've lost both your device and your recovery codes, ask the staff:
people who can manage users can turn two-factor login off for you
(**Admin → Users → Turn off 2FA**), which is recorded in the moderation
log.

[API keys](../api.md) don't need a code: keep them secret, and revoke any
you no longer use. Making one asks for your password (or, on an account
made through single sign-on, which has none, works only within 10
minutes of logging in), and a key can't make more keys or change your
login settings.

## Saved searches

Save a search from its results (**Save this search**, beside the
results), or under **Settings → Saved searches**, optionally with labels.
Then `search:all` shows the newest posts of all your saved searches
together, and `search:artists` those of the searches labelled `artists`;
both combine with other tags and filters, like `search:all rating:g`.
Each saved search adds up to its newest 500 posts, and a `search:` term
runs at most 20 saved searches. Only you see your saved searches.

## Favorite groups

Favorite groups are your own named lists of posts, in the order you
choose: make one under **Your favorite groups** (linked from your
profile) or from a post page, add posts from their pages, and reorder
them by dragging on the group's edit page. A group is public (listed on
your profile, and anyone can open it) unless you untick *Public*.
`favgroup:name` searches one of your groups, `favgroup:7` any public
group by number, and `ordfavgroup:name` shows a group in its own order.
