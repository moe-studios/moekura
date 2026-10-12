# Bots

A bot is an account run by a program, such as an uploader mirroring
another site, a tagging script or a chat bot, rather than a person. It
uses the site only through [API keys](../api.md#authenticating):

- it can't log in on the site, whether with a password, single sign-on
  or a passkey, and a password reset isn't sent to it;
- a [feed](../using/search.md#feeds) token doesn't work for it;
- it isn't [promoted automatically](roles.md#automatic-promotion);
- nobody can send it messages, since nobody would read them;
- its profile and the admin user list mark it **Bot**, and the API's
  user answers have `"is_bot": true`.

Otherwise it's an ordinary account: its role decides what it may do,
its uploads and edits are credited to it, and it can be banned.

## Setting one up

Make the account, then a key for it, from the shell:

```sh
moekura admin create-user mirrorbot --bot --role contributor
moekura admin create-api-key mirrorbot --name "mirror" \
  --permission view_posts --permission upload --permission edit_posts
```

The key is printed once; give it to the program. Limiting it to the
permissions the program needs means a leaked key gives away less. List
and revoke the bot's keys with `moekura admin api-keys mirrorbot` and
`moekura admin revoke-api-key mirrorbot ID`: with no way to log in, a
bot has no settings page of its own.

`moekura admin set-bot NAME` makes an existing account a bot, ending
its sessions and revoking its feed token but keeping its API keys; `--off` makes it a person's
again, who then logs in by resetting the password or through single
sign-on.
