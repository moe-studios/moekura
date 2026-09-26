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

## Who can register

Registration is open by default. Change it under **Admin → Settings**, or
from the shell:

```sh
moekura admin settings set registration_mode closed
```

| Mode | Who can create an account |
|---|---|
| `open` | anyone |
| `invite` | people with an invite code (`moekura admin create-invite`) |
| `approval` | anyone, but staff approve new accounts before they can log in (**Admin → Users**, filter *pending*) |
| `closed` | nobody; admins create accounts from the shell |

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

To show nothing at all without logging in, make the site
[private](../admin/private-sites.md).

## Link previews

Links to posts, pools and wiki pages unfurl in chat apps and social
networks (OpenGraph and Twitter card tags), and posts have
[oEmbed](https://oembed.com/) at `/oembed?url=…`. Previews show a
post's image only for general and sensitive posts, unless you tick
**Link previews … show questionable and explicit posts' images too** in
the site settings (or `moekura admin settings set preview_all_ratings
true`). Private sites show no previews at all.

## Reviewing uploads

When **New uploads wait for approval** is ticked under **Admin → Settings**, uploads by users without the *Upload without approval*
permission wait in the approval queue (**Moderation**). See [Moderation](../admin/moderation.md).
