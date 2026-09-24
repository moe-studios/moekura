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

## What visitors see

Visitors see every active post by default. To hide some unless people opt
in, set a default blacklist, which applies to visitors and to users who
haven't set their own:

```sh
moekura admin settings set default_blacklist "rating:e"
```

To show nothing at all without logging in, make the site
[private](../admin/private-sites.md).

## Reviewing uploads

When **New uploads wait for approval** is ticked under **Admin → Settings**, uploads by users without the *Upload without approval*
permission wait in the approval queue (**Moderation**). See [Moderation](../admin/moderation.md).
