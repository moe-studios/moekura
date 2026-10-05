## Accounts: logging in, signing up, settings

account-name = Name
feed-token-title = Your feed token
feed-token-copy = Copy it now: it isn't shown again. Making a new one stops the old one working.
feed-token-hint = Add <code>&amp;token=…</code> to a feed's address to read it as you, for example:
resend-title = Confirm your email address
resend-intro = Enter your account name, and we'll send the link to confirm your address again.
resend-send = Send the link
login-code-title = Two-factor login
login-code-heading = Enter your code
login-code-field = Code from your authenticator app
login-code-hint = Lost your device? Enter one of your recovery codes instead.

account-email = Email
account-password = Password
password-new = New password
password-repeat = Repeat it
password-change = Change password
reset-title = Choose a new password
reset-hint = This logs you out everywhere, so you'll log in again with the new password.
forgot-title = Forgot your password?
forgot-sent = If an account uses <strong>{ $email }</strong>, we've sent it a link to choose a new password. The link works for an hour.
forgot-back = Back to logging in
forgot-intro = Enter the email address of your account, and we'll send you a link to choose a new password.
login-resend = Send it again
login-no-account = No account yet?
login-captcha = This account has had many login attempts lately, so logging in to it needs a captcha for now.
name-title = Your name
name-current = You're <strong>{ $name }</strong>.
name-wait = You can change your name once every { $days } days; you can next change it on { $next }.
name-new = New name
name-hint = Letters, digits, <code>_</code>, <code>.</code> and <code>-</code>. You log in with it from now on. Your old name stays on your profile, and links and searches using it keep finding you, unless someone else takes it. You can change it again after { $days } days.
name-change = Change name

register-title = Create an account
register-approval = New accounts are reviewed by the staff before they can log in.
register-name-hint = Letters, digits, underscores, dots and hyphens.
register-email-hint = We'll send you a link to confirm it before you can log in.
register-repeat = Repeat password
register-invite = Invite code
register-rules = By registering, you agree to follow the site's <a href="/rules">rules</a>.
register-have-account = Already registered?

account-email-password = Email and password
tfa-save-codes = <strong>Save these recovery codes somewhere safe.</strong> Each one logs you in once if you lose your device. They won't be shown again.
tfa-saved = I've saved them
tfa-scan = Scan this with an authenticator app, then enter the code it shows.
tfa-qr = QR code for your authenticator app
tfa-cant-scan = Can't scan it? Enter this key in the app instead: <code class="secret">{ $secret }</code>
tfa-code = Code
tfa-on = Two-factor login is <strong>on</strong>: after your password, you enter a code from your authenticator app. You have { $count } unused recovery { $count ->
    [one] code
   *[other] codes
}.
tfa-new-codes = Make new recovery codes
tfa-off-confirm = Turn off two-factor login?
tfa-off = Two-factor login is <strong>off</strong>. Turn it on to need a code from an authenticator app, as well as your password, to log in.
tfa-set-up = Set it up

invites-title = Invites
invites-not-needed = Signing up doesn't need an invite at the moment, but codes keep working if that changes.
invites-created = Your new invite. Copy it now: it won't be shown again.
invites-link = Or send this link, which signs up with it: <code class="secret">{ $link }</code>
invites-new = New invite
invites-unlimited = You can make as many as you like.
invites-quota = You can make { $quota } every { $days } days, each for one person; { $remaining } left now.
invites-uses = Uses
invites-days = Lasts this many days
invites-days-hint = 0 for an invite that never expires.
invites-note = Note
invites-note-placeholder = Who it's for
invites-make = Make an invite
invites-table = Invites table
invites-made = Made
invites-expires = Expires
invites-used-by = Used by
invites-uses-of = { $uses } of { $max }
invite-state-open = open
invite-state-used_up = used up
invite-state-expired = expired
invite-state-revoked = revoked
invites-revoke-confirm = Revoke this invite? Its code stops working.
invites-none = No invites yet.

account-email-heading = Email address
account-email-is = Your address is <strong>{ $email }</strong>.
account-email-confirmed = Your address is <strong>{ $email }</strong> (confirmed).
account-email-unconfirmed = Your address is <strong>{ $email }</strong> (not confirmed yet).
account-no-email = You haven't given an email address.
account-no-email-mail = You haven't given an email address, so you can't reset your password if you forget it.
account-new-address = New address
account-email-hint = We'll send a link to the new address; it replaces the old one once you follow it. Leave it empty to remove your address.
account-change-address = Change address
account-current-password = Current password
account-repeat-new = Repeat the new password
account-password-hint = Changing it logs you out everywhere else.
account-sso = Single sign-on
account-sso-linked = Linked to your account at <strong>{ $provider }</strong> on { $date }
account-unlink-confirm = Unlink this login?
account-unlink = Unlink
account-sso-no-password = Your account has no password: you log in through the provider.
account-sso-link-intro = Link your account to log in with <em>{ $provider }</em> instead of your password.
account-link = Link
account-tfa-intro = Ask for a code from an authenticator app too when logging in. <a href="/settings/two-factor">Two-factor login settings</a>
api-keys-title = API keys
api-keys-intro = Scripts and apps use API keys to act as you through the <a href="{ $url }">API</a>. Send one as <code>Authorization: Bearer &lt;key&gt;</code>. A key can do everything you can, so keep it secret, and revoke it if it leaks.
api-keys-created = Your new key <strong>{ $name }</strong>. Copy it now: it won't be shown again.
api-keys-table = API keys table
api-key = Key
api-keys-created-heading = Created
api-keys-last-used = Last used
api-keys-expired = expired { $day }
api-keys-revoke-confirm = Revoke this key? Scripts using it stop working.
api-keys-none = You have no API keys.
api-keys-new = New key
api-keys-name-placeholder = What will use it
api-keys-expiry-never = Never
api-keys-expiry-30 = In 30 days
api-keys-expiry-90 = In 90 days
api-keys-expiry-365 = In a year
api-keys-create = Create key

settings-per-page = Posts per page
settings-site-default = Site default ({ $value })
settings-theme = Theme
settings-mode-system = Match your device
settings-mode-light = Light
settings-mode-dark = Dark
settings-blacklist = Blacklist
settings-blacklist-hint = Posts matching a line are hidden. One rule per line; every term on a line must match: <code>tag</code>, <code>-tag</code>, <code>rating:e,q</code>.
settings-blur = Blur blacklisted posts instead of leaving them out
settings-display = Display
settings-safe-mode = Safe mode: only show general-rated posts
settings-original = Show original images on post pages, not resized ones
settings-show-deleted = Include deleted posts in searches
settings-large-thumbs = Large thumbnails ({ $size } pixels)
settings-hide-comments = Hide comments on post pages
settings-email-notifications = Email me my notifications (to a confirmed address)
settings-autocomplete = Suggest tags while typing
settings-time-zone = Time zone
settings-time-zone-hint = For the dates shown on pages.
settings-language = Language
settings-language-browser = Your browser's
settings-custom-css = Custom CSS
settings-custom-css-hint = Applied after the site's styles, on every page, for you alone.
settings-account-link = Your email address and password
settings-profile-link = Your profile picture, banner and bio
settings-api-keys = <a href="/settings/api-keys">API keys</a> let scripts and apps use the site as you.
settings-invites = <a href="/invites">Invites</a> let people sign up when it takes an invite code.
settings-feeds = Feeds
settings-feeds-hint = Search results and comments have Atom feeds (the <em>Feed</em> link beside results). On a private site, feed readers need your feed token: it lets them read feeds as you, and nothing else.
settings-feed-token-new = Make a new feed token
settings-feed-token-make = Make a feed token
settings-feed-token-revoke-confirm = Revoke your feed token? Feed readers using it stop working.
settings-feed-token-revoke = Revoke it
settings-saved-searches = <a href="/saved_searches">Saved searches</a>, to see the newest posts of several searches together.
