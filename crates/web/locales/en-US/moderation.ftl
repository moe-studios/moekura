## Moderation pages

mod-nav-queue = Approval queue
mod-nav-flags = Flags
mod-nav-appeals = Appeals
mod-nav-comments = Reported comments
mod-nav-held = Held for review
mod-nav-dmails = Reported messages
mod-nav-deleted = Deleted posts
mod-nav-purge = Purge
mod-nav-changes = Post changes
mod-nav-mass-edit = Mass edit
mod-nav-bans = Bans
mod-nav-log = Log

mod-dmails-hint = Messages their recipients reported. Staff see these, and no others.
mod-dmails-from-to = from { $sender } to { $recipient }
mod-dmails-settled = Settled
mod-dmails-reported = Reported: { $reason }
mod-dmails-none = No reported messages.

mod-appeals-hint = Deleted posts someone asked to bring back (<a href="{ $url }">status:appealed</a>), oldest appeal first. Restoring a post grants its appeal.
mod-appeals-why = Why it stays deleted
mod-appeals-why-post = Why #{ $id } stays deleted
mod-appeals-keep = Keep deleted
mod-appeals-none = No open appeals.
mod-comments-why = Reason for hiding comment #{ $id }
mod-comments-hide = Hide comment
mod-comments-none = No reported comments.
mod-held-hint = Comments, forum posts and messages the spam filter held. Nobody else sees them until approved; rejected ones stay hidden, and rejected messages never arrive.
mod-held-off = The filter is off (<a href="/admin/settings">Admin → Settings → Spam</a>).
held-what-comment = A comment on post #{ $id }
held-what-forum-post = A forum post in “{ $title }”
held-what-dmail = A message to { $user }: “{ $title }”
# The reason was written when it was held.
mod-held-reason = Held: { $reason }
mod-held-none = Nothing is held.
mod-flags-title = Flagged posts
mod-flags-none = No open flags.

## The moderation log: what each action says, after who did it

action-post_approve = approved post
action-post_reject = rejected post
action-post_delete = deleted post
action-post_restore = restored post
action-post_purge = purged post
action-flag_dismiss = dismissed flags on post
action-user_ban = banned
action-user_unban = unbanned
action-ip_ban = banned addresses
action-ip_unban = unbanned addresses
action-user_role = changed the role of
action-user_status = changed the status of
action-user_two_factor_reset = turned off two-factor login for
action-role_update = updated a role
action-setting_update = changed a site setting
action-tag_update = edited a tag
action-tag_relation_approve = approved a tag relation
action-tag_relation_reject = rejected a tag relation
action-tag_relation_remove = removed a tag relation
action-job_retry = retried a job
action-job_discard = discarded a job
action-comment_hide = hid a comment on post
action-comment_restore = restored a comment on post
action-comment_report_dismiss = dismissed reports about a comment on post
action-comment_sticky = pinned a comment on post
action-comment_unsticky = unpinned a comment on post
action-pool_delete = deleted a pool
action-pool_undelete = restored a pool
action-user_promote = promoted
action-tags_mass_update = started a mass tag edit
action-bulk_update_approve = approved a bulk update request
action-bulk_update_reject = rejected a bulk update request
action-role_create = added a role
action-role_delete = deleted a role
action-appeal_reject = turned down an appeal of post
action-post_lock = changed the locks on post
action-post_versions_undo = undid the post edits of
action-artist_ban = banned an artist
action-artist_unban = unbanned an artist
action-post_replace = replaced the file of post
action-posts_delete_uploads = started deleting the uploads of
action-posts_purge_batch = started purging deleted posts
action-dmail_report_settle = settled a report about a message from
action-forum_topic_moderate = changed a forum topic
action-forum_topic_merge = merged forum topics
action-forum_post_hide = hid a forum post by
action-forum_post_unhide = restored a forum post by
action-held_approve = let through writing held as spam, by
action-held_reject = turned away writing held as spam, by
action-user_feedback_delete = deleted feedback on
action-user_feedback_restore = restored feedback on
action-user_rename = renamed
action-news_post = posted site news
action-news_delete = deleted site news

mod-log-title = Moderation log
mod-log-action = Action
mod-log-none = Nothing logged.
mass-title = Mass edit tags
mass-hint = Add and remove tags on every post a search finds, in the background. Changes show in each post's history, and new tags bring what they imply. Preview first.
mass-hint-metatags = The tags to add can also take <code>-tag</code> to remove a tag and <code>rating:g</code>, <code>s</code>, <code>q</code> or <code>e</code> to set the rating; locked tags and ratings are left alone.
mass-add = Add tags
mass-remove = Remove tags
mass-change = Change { $count }
mass-adding = Adding { $tags }.
mass-removing = Removing { $tags }.
mass-rating = Rating them { $rating }.
mass-recent = Recent mass edits
mass-table = Mass edit table
mass-progress = { $changed } changed of { $seen }
mass-none = No mass edits yet.

profile = Profile
record-account = Account
role = Role
joined = Joined
last-seen = Last seen
never = never
unconfirmed = (unconfirmed)
auto-promotion = Automatic promotion
promotion-kept = kept from it
promotion-allowed = allowed
ban = Ban
ban-change = Change the ban
ban-replaces = The new ban replaces the one in force.
ban-for = For
ban-for-1 = 1 day
ban-for-3 = 3 days
ban-for-7 = 1 week
ban-for-30 = 1 month
ban-for-365 = 1 year
ban-for-ever = Until lifted
ban-user = Ban { $user }
ban-lift = Lift the ban
ban-lifted = lifted
never-banned = Never banned.
addresses = Addresses
addresses-table = Addresses table
address = Address
first-seen = First seen
actions = Actions
ban-address = Ban address
addresses-none = No addresses recorded.
addresses-others = Other accounts on these addresses
addresses-other-on = on <code>{ $ip }</code>, last { $last }
uploads = Uploads
upload-count = { $count } { $count ->
    [one] upload
   *[other] uploads
}
recently-deleted = Recently deleted
deleting-uploads = Deleting all uploads
batch-deleted = { $done } of { $total } deleted
batch-skipped = , { $count } skipped
batch-failed = , { $count } failed
delete-all-uploads = Delete all { $count } { $count ->
    [one] upload
   *[other] uploads
}
delete-all-uploads-hint = Deletes every active, flagged and pending post { $user } uploaded, in the background, with one reason; open flags on them are upheld. Each deletion is logged as yours, and the posts can be restored. The account stays.
delete-uploads-confirm = Delete { $count } { $count ->
    [one] upload
   *[other] uploads
} by { $user }
delete-all-uploads-button = Delete all uploads
flags-received = On their uploads: { $count }
flags-filed = Filed by them: { $count }
disapprovals = Pending posts they disapproved: { $count }
disapproval-breaks_rules = breaks rules
disapproval-poor_quality = poor quality
disapproval-disinterest = disinterest
comment-reports = Reports about their comments: { $count }
hidden-comments = Hidden comments
report-count = { $count } { $count ->
    [one] report
   *[other] reports
}
log-about = Everything about { $user }
log-about-none = Nothing logged about { $user }.
log-by-heading = By { $user }
log-by = Everything by { $user }

purge-title = Purge deleted posts
purge-hint = Purging removes deleted posts, their files and their history for good, in the background; it can't be undone. Search the deleted posts (<code>user:name</code>, tags and so on; <code>status:deleted</code> is implied), or leave the search empty for all of them, then purge those you tick or every match.
purge-placeholder = user:name, tags…
purge-none-match = No deleted posts match.
tick-every-shown = Tick every post shown
tick-every-page = Tick every post on this page
tick-post = Tick #{ $id }
purge-confirm = Remove these posts, their files and their history for good
purge-ticked-confirm = Purge the ticked posts for good?
purge-ticked = Purge ticked
purge-all-confirm = Purge all { $count } for good?
purge-all = Purge all { $count }
purge-limit = Up to { $max } ticked posts at once. Posts restored before the purge gets to them are left alone.
purge-recent = Recent purges
purge-table = Recent purges table
purge-ticked-count = { $count } ticked { $count ->
    [one] post
   *[other] posts
}
purge-progress = { $done } of { $total } purged
purge-none = No purges yet.
bans-table = Banned users table
until = Until
bans-hint = Ban or unban users from their profile pages.
bans-none = No one is banned. Ban users from their profile pages.
networks = Networks
networks-hint = Addresses in a partly banned network can look around but can't register, log in, post or change anything; a full ban keeps them from seeing the site at all.
networks-table = Banned networks table
network = Network
ban-full = full
ban-partial = partial
lift = Lift
ban-network-title = Ban a network
ban-network-field = Address or range
ban-network-placeholder = 203.0.113.7, 203.0.113.0/24 or 2001:db8::/64
ban-partial-option = Partial: can look, can't register, log in or change anything
ban-full-option = Full: can't see the site
ban-network = Ban network
queue-hint = Pending posts you didn't upload and haven't disapproved (<a href="{ $url }">status:unmoderated</a>). Disapproving passes on a post without rejecting it: it leaves your queue, and other approvers see why.
queue-placeholder = tags, user:name, rating:e…
order = Order
queue-order-id_asc = Oldest first
queue-order-id = Newest first
queue-order-score = Highest score
queue-order-favcount = Most favorites
queue-order-tagcount_asc = Fewest tags
queue-order-mpixels = Largest
queue-order-filesize = Largest file
queue-bulk-hint = Tick posts below to approve or reject them together.
approve-ticked = Approve ticked
reject-ticked = Reject ticked
reason-reject-ticked = Reason for rejecting the ticked posts
reason-reject-post = Reason for rejecting #{ $id }
no-tags = no tags
disapproved = disapproved
disapproval-reason-breaks_rules = Breaks the rules
disapproval-reason-poor_quality = Poor quality
disapproval-reason-disinterest = Not interested
disapprove-why = Why you pass on #{ $id }
disapprove-choose = Pass on it…
disapprove-note = Note for other approvers
disapprove-note-post = Note on #{ $id } for other approvers
disapprove = Disapprove
queue-none = Nothing waiting for approval.
queue-none-match = Nothing waiting for approval matches that search.
