# The language's name, in itself, for the language menu.
language-name = English

common-save = Save

## Messages shown once, after an action

flash-logged_in = Welcome back!
flash-logged_out = You have been logged out.
flash-registered = Your account is ready. Welcome!
flash-awaiting_approval = Your account was created and is waiting for approval by the staff.
flash-saved = Saved.
flash-api_key_revoked = The API key was revoked.
flash-check_email = We've sent you an email. Follow the link in it to confirm your address.
flash-email_confirmed = Your email address is confirmed.
flash-password_changed = Your password was changed.
flash-some_skipped = Done. Some had been dealt with meanwhile, and were skipped.
flash-queued = Started; it runs in the background.
flash-held = Thanks! It looked like it might be spam, so the staff will check it before anyone sees it.
flash-duplicate = Duplicate: that file was already uploaded as this post.

optional = (optional)
back-to-settings = Back to settings
post-number = Post #{ $id }
markup-help = <code>[b]bold[/b]</code>, <code>[i]italic[/i]</code>, <code>[quote]</code>…<code>[/quote]</code> on lines of their own,
    <code>[[tag]]</code> for a wiki page, <code>post #123</code>, <code>comment #45</code>, <code>@name</code> to notify someone, and plain links.
chart-label = { $label }, at most { $most }
card-alt = Post #{ $id }
card-alt-tags = { $tags }, post #{ $id }
card-alt-processing = Post #{ $id } (still processing)
card-blacklisted = blacklisted
card-video = video
card-animated = anim
card-sound = with sound
card-pending = pending
card-flagged = flagged
card-deleted = deleted
card-has-parent = has a parent
card-has-children = has children

error-back = Back to the front page
error-not-found = Not found
error-unauthorized = You need to log in first
error-forbidden = You don't have permission to do that
error-duplicate = This file was already uploaded
error-too-many = Too many attempts. Please wait a moment and try again
error-internal = Something went wrong on our side

rules-title = Rules
rules-edit = Edit the rules

stats-counted = Counted hourly; last on { $day }.
stats-not-yet = The site's figures aren't counted yet; they will be within the hour.
stats-reports = <a href="/reports">Reports</a> chart what's happened over time.
stats-posts = Posts
stats-tags = Tags in use
stats-users = Users
stats-favorites = Favorites
stats-comments = Comments
stats-forum_posts = Forum posts
stats-wiki_pages = Wiki pages
stats-pools = Pools
stats-artists = Artists
stats-notes = Notes

field-title = Title
edit-thing = Edit { $name }
edit-link = Edit <a href="{ $url }">{ $name }</a>
other-names = Other names
reason = Reason
approve = Approve
reject = Reject
withdraw = Withdraw
remove = Remove
requested = requested { $date }
requested-by = requested { $date } by { $user }
decided-by = decided by { $user }
posts-in-order = Posts, in order
posts-in-order-hint = Post numbers separated by spaces.
posts-drag-hint = Drag the thumbnails below to reorder them.

reason-choose = Choose a reason
reason-none = No reason
reason-other = Other (say why)
reason-details = Details
reason-details-optional = (optional with a reason above)

## Statuses, as `status-<status>`

status-pending = pending
status-approved = approved
status-rejected = rejected
status-active = active
status-deleted = deleted
status-processing = processing
status-queued = queued
status-failed = failed
status-done = done

url = URL
search = Search
edit = Edit
delete = Delete
pages = Pages
older = Older ›
newer = ‹ Newer
previous = Previous
next = Next
private = (private)
current = (current)
gone = (gone)
last-changed = Last changed
by-user = by { $user }
version = Version { $number }
history-of = History of { $name }
revert-to-this = Revert to this
characters-changed = { $sign }{ $change } characters
post-count = { $count } { $count ->
    [one] post
   *[other] posts
}
staff-notes = Staff notes
staff-notes-hint = Only staff see these.
staff-notes-add = Add a note
staff-notes-add-button = Add note

all = All
more = More
user = User
someone = Someone
deleted-user = Deleted user
description = Description
description-changed = description changed
restored = restored
reordered = reordered
restore = Restore
dismiss = Dismiss
public = public
private-word = private
score = Score
votes = Votes
vote-for = Vote for
vote-against = Vote against
by-link = by <a href="{ $url }">{ $user }</a>
rating-is = rating { $rating }
deleted-when = Deleted { $when }
deleted-when-by = Deleted { $when } by { $user }
reason-for-deleting = Reason for deleting #{ $id }
delete-post = Delete post
discussion = Discussion
discussion-forum = Discussed on the forum: <a href="/forum_topics/{ $id }">topic #{ $id }</a>.
discussion-votes-hint = Votes help staff decide; they don't decide by themselves.

by = By
words = Words
show = Show
filter = Filter
categories = Categories
none = none
none-parenthesized = (none)
edited = edited
created = created
older-plain = Older
version-lower = version { $number }
changes-none = No changes found.
reply = Reply
report = Report
report-to-staff = Report to the staff
report-whats-wrong = What's wrong with it?
report-placeholder = Spam, harassment, …
block-user = Block { $user }
unblock-user = Unblock { $user }
banned-word = banned
unbanned = unbanned

state = State
status = Status
any = Any
on = on
off = off
tag = Tag
request = Request
requested-heading = Requested
version-heading = Version
turn-on = Turn on
turn-off = Turn off
your-password = Your password
confirm-fresh-login = Your account has no password to confirm this with, so it works only within { $minutes } minutes of logging in. If it's been longer, log out and log in again first.
renamed-from = renamed from { $name }
deprecated = deprecated
no-longer-deprecated = no longer deprecated

## Dates. Months are numbered from 1.

date-long = { $day } { $month } { $year }
date-month = { $month } { $year }
month-1 = January
month-2 = February
month-3 = March
month-4 = April
month-5 = May
month-6 = June
month-7 = July
month-8 = August
month-9 = September
month-10 = October
month-11 = November
month-12 = December

from = From
moved = moved
retry = Retry
discard = Discard
preview = Preview
changes = Changes
progress = Progress
started = Started
on-capital = On
command-line = command line
status-running = running
status-dead = failed
status-flagged = flagged
status-open = open
status-dismissed = dismissed
status-upheld = upheld
status-applying = applying
status-applied = applied
status-withdrawn = withdrawn
status-delivered = delivered
status-unverified = unverified
status-deactivated = deactivated

recent-wiki_page_versions = Wiki changes
recent-pool_versions = Pool changes
recent-note_versions = Note changes
note-version-lower = note { $note }, version { $version }

date = Date
hide = Hide
hidden = hidden
unhide = Unhide
unblock = Unblock
new-badge = new
moderate = Moderate
vote-up = Vote up
vote-down = Vote down
score-lower = score
edited-by = edited by { $user }
reason-for-log = Reason (for the log)

yes = Yes
no = No
updated = Updated
inactive = inactive
none-yet = None yet.
all-posts = All posts »
unban = Unban
revoke = Revoke
recent-changes = Recent changes

either = Either
save-thing = Save { $name }
delete-thing = Delete { $name }
reason-optional = Reason (optional)

add = Add
added = Added
removed = Removed
nothing-yet = Nothing here yet.
source = Source
rating = Rating
rating-g = General
rating-s = Sensitive
rating-q = Questionable
rating-e = Explicit
upload-word = upload

or = or
feed = Feed
none-capital = None
rename = Rename
rename-user = Rename { $user }

api-auth = Authentication
api-auth-intro = Create an API key on your <a href="/settings/api-keys">API keys</a> page and send it with every request:
api-auth-hint = A key acts as you, with your role's permissions; while you're banned it can only read. The machine-readable description of everything below is at <a href="{ $url }">{ $url }</a> (OpenAPI 3.1), for generating clients.
api-endpoints = Endpoints
api-schemas = Schemas
api-parameters = Parameters
api-in = In
api-type = Type
api-required = required
api-body = Request body
api-body-as = { $type } as <code>{ $content_type }</code>
api-responses = Responses
api-field = Field
