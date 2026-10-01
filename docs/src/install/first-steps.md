# First steps

## The first admin

Create the first account from the shell. It asks for a password (or reads
one line from standard input when piped):

```sh
moekura admin create-user yourname --role admin
# with compose:
docker compose -f deploy/compose.tiny.yml exec app moekura admin create-user yourname --role admin
```

Log in, then open **Admin** in the menu.

## Name, logo and rules

Under **Admin → Settings**:

- **Site name** and **Description** show in the header, the footer and to
  search engines (`<meta name="description">`).
- **Logo**: an image (PNG, JPEG, GIF, WebP or AVIF, up to 1 MB) shown
  beside the name in the header.
- **Rules**: what may be posted and how to behave, in the same
  [markup](../using/wiki.md) as the wiki. They're shown at `/rules`, which
  anyone can read (on private sites too), and linked from the footer, the
  sign-up form and the upload page. Leave them empty for no rules page.
- **Footer links**: one per line, the link's text then its address, like
  `Discord https://discord.gg/abc` or `Help /wiki/help`.

From the shell, the footer links are a JSON list:

```sh
moekura admin settings set footer_links '[{"label": "Discord", "url": "https://discord.gg/abc"}]'
```

## Who can register

Registration is open by default. Change it under **Admin → Settings**, or
from the shell:

```sh
moekura admin settings set registration_mode closed
```

| Mode | Who can create an account |
|---|---|
| `open` | anyone |
| `invite` | people with an invite code (see [Invites](#invites)) |
| `approval` | anyone, but staff approve new accounts before they can log in (**Admin → Users**, filter *pending*) |
| `closed` | nobody; admins create accounts from the shell |

## Invites

Invite codes are made at **Settings → Invites** (`/invites`), or with
`moekura admin create-invite`. Each code comes with a sign-up link that
fills it in, and is shown only once. The list shows each invite's uses,
expiry and the accounts made with it, which also show **Invited by** on
their profiles.

Roles with *Invite people* (moderators, by default) make single-use
invites lasting up to 30 days, up to **Invites each person may make
every 30 days** in **Admin → Settings** (5 to start with), and revoke
their own. Those who can manage users have no limit, choose how many
times an invite can be used and how long it lasts (or that it never
expires), and see and revoke everyone's.

## Email

With [`[mail]`](../configuration.md#mail) set up, people can reset a
forgotten password from the login page, and confirm their address under
**Settings → Your email address and password**. Without it, those pages
don't appear, and people who forget their password need an admin.

To make new accounts confirm their address before they can log in, tick
**New accounts must confirm their email address** under **Admin →
Settings**, or:

```sh
moekura admin settings set email_verification true
```

Registering then needs an address. Accounts waiting for the link are
*unverified* under **Admin → Users**, where you can also activate one by
hand. With `approval` registration, confirming the address puts the
account in the approval queue.

## What visitors see

Visitors see every active post by default. To hide some unless people opt
in, set a default blacklist, which applies to visitors and to users who
haven't set their own:

```sh
moekura admin settings set default_blacklist "rating:e"
```

A blacklist only hides posts until someone turns it off. To keep
visitors from seeing some ratings at all, in searches, on post pages, in
feeds and through the APIs, limit the ratings they see (logged-in users
still see everything):

```sh
moekura admin settings set visitor_ratings '["g", "s"]'
```

To show nothing at all without logging in, make the site
[private](../admin/private-sites.md).

## Link previews

Links to posts, pools and wiki pages unfurl in chat apps and social
networks (OpenGraph and Twitter card tags), and posts have
[oEmbed](https://oembed.com/) at `/oembed?url=…`. Video posts include
OpenGraph video URLs, formats and dimensions for inline playback in Discord,
with their poster as the thumbnail. The site and media URLs must be publicly
reachable; playback depends on the client's support for the original MP4 or
WebM file. Previews show a post's image or video only for general and
sensitive posts, unless you tick
**Link previews … show questionable and explicit posts' images too** in
the site settings (or `moekura admin settings set preview_all_ratings
true`). Private sites show no previews at all.

## Reviewing uploads

When **New uploads wait for approval** is ticked under **Admin → Settings**, uploads by users without the *Upload without approval*
permission wait in the approval queue (**Moderation**). See [Moderation](../admin/moderation.md).
