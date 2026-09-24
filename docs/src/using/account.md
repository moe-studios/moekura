# Your account

Everything here is under **Settings → Your email address and password**.

## Email address and password

Changing either needs your current password. Changing the password logs
you out everywhere else.

On sites that send mail, a new address only replaces the old one once
you follow the link sent to it, and **Forgot your password?** on the
login page emails you a link to choose a new one. The link works for an
hour, and using it logs you out everywhere.

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
your password. If your device's clock is off by more than about half a
minute, codes won't work; most phones set the time automatically.

If you've lost both your device and your recovery codes, ask the staff:
people who can manage users can turn two-factor login off for you
(**Admin → Users → Turn off 2FA**), which is recorded in the moderation
log.

[API keys](../api.md) don't need a code: keep them secret, and revoke any
you no longer use.
