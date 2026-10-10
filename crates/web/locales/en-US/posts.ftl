## Posts, favorites and favorite groups

fav-group-new = New favorite group
fav-groups-yours = Your favorite groups
fav-group-public = Public: anyone can see it

fav-groups-of = { $user }'s favorite groups
fav-group-new-button = New group
fav-groups-table = Favorite groups table
fav-groups-none = No favorite groups.
fav-groups-none-yet = No favorite groups yet.
image-search-title = Search by image
image-search-file = A picture
image-search-url = …or a link to one
image-search-hint = Finds posts that look like it, even resized or recompressed, without uploading anything. A work's page on Pixiv, X and the like works too.
image-search-like-post = Posts like #{ $id }
image-search-like-it = Posts like it
image-search-alike = { $percent }% alike
image-search-none = No posts look like it.
note-history-title = Note history of post #{ $id }
note-history-all = <a href="/note_versions">All note changes</a>, with a filter by user.
note-version = Note { $note }, version { $version }
note-history-none = This post has no notes.
metadata-title = Metadata of post #{ $id }
metadata-table = Post metadata table
metadata-field-posts = Posts with this field
metadata-value-posts = Posts with this value
metadata-hint = Locations, serial numbers and owners' names aren't shown. The original file is kept as uploaded, with whatever it contains.
metadata-none = This file has no metadata to show, or it hasn't been read yet.
replacements-title = Replacements of post #{ $id }
replacements-from = from { $source }
replacements-none = This post's file was never replaced.

fav-group-empty = This group has no posts yet.
fav-group-delete-confirm = Delete this favorite group? The posts stay, but the group and its order are gone.
fav-group-delete = Delete group

post-history-title = History of post #{ $id }
post-history-hint = <a href="/post_versions">All post changes</a>, with filters by user, tag and date. Changes to the file itself are under <a href="/posts/{ $id }/replacements">replacements</a>.
history-relation-alias = alias { $antecedent } → { $consequent }
history-relation-implication = implication { $antecedent } → { $consequent }
history-rating = rating: { $rating }
history-source = source: { $source }
history-parent = parent: { $parent }
history-locked = locked: { $locks }
history-none = No history recorded.

explore = Explore
explore-heading-popular = Popular posts
explore-heading-viewed = Most viewed posts
explore-heading-searches = Popular searches
explore-heading-missed_searches = Searches that found nothing
explore-viewed = Most viewed
explore-searches = Searches
explore-missed = Missed searches
explore-week = { $from } to { $to }
explore-dates = Dates
explore-earlier = « Earlier
explore-later = Later »
scale-day = Day
scale-week = Week
scale-month = Month
explore-missed-hint = Tag searches that found no posts: often a misspelling or another name for a tag, which an alias could fix.
explore-table = Explore table
explore-times-missed = Times missed
explore-no-searches = No searches counted then.
explore-none-viewed = No posts viewed then.
explore-none-posted = No posts posted then.

saved-title = Saved searches
saved-hint = <code>search:all</code> shows the newest posts of all your saved searches together, and
    <code>search:label</code> those of the searches with that label.
saved-all = All together
saved-table = Saved searches table
saved-labels = Labels
saved-none = You haven't saved any searches yet.
saved-new = Save a search
saved-labels-hint = (optional, separated by spaces)
saved-max = Up to { $max } searches.

undo-title = Undo { $user }'s edits
undo-from = { " " }from { $day }
undo-until = { " " }until { $day }
undo-hint = Takes back every post edit { $user } made{ $range }, in the background: tags they added come off and tags they removed go back, and a rating, source, description or parent they set is restored where nobody changed it since. Uploads stay. Each post's history credits you.
undo-confirm = Undo every post edit { $user } made in this range?
undo-button = Undo their edits
upload-rules = Check the <a href="/rules">rules</a> for what may be uploaded.
upload-left-both = You can upload { $pending } more before some are approved, and { $today } more today.
upload-left-pending = You can upload { $pending } more before some are approved.
upload-left-today = You can upload { $today } more today.
upload-duplicate = This file was already uploaded as <a href="/posts/{ $id }">post #{ $id }</a>.
upload-file = File
upload-files = Files
upload-new = New upload
uploads-mine = My uploads
uploads-hint = Files you uploaded, newest first. Files not posted within a day are removed.
uploads-none = You haven't uploaded anything yet.
uploads-none-found = No files match.
uploads-filter = Filter
uploads-status = Status
uploads-status-pending = Downloading
uploads-status-ready = Ready
uploads-status-failed = Failed
uploads-posted = Posted
uploads-posted-yes = Posted
uploads-posted-no = Not posted
uploads-any = Any
uploads-uploader = Uploader
uploads-everyone = everyone
upload-not-yours = This is { $name }'s upload: only they can post it.
upload-title = Upload #{ $id }
upload-file-title = File { $number } of { $count }
upload-files-nav = Files of this upload
upload-summary = { $count ->
    [one] 1 file
   *[other] { $count } files
}, { $posted } posted.
upload-downloading = { $count ->
    [one] 1 is still downloading…
   *[other] { $count } are still downloading…
}
upload-no-files = This upload has no files.
upload-file-downloading = Downloading…
upload-file-failed = Failed
upload-file-posted = Posted
upload-file-post = Post it
upload-file-was-posted = This file was posted as <a href="/posts/{ $id }">post #{ $id }</a>.
upload-no-preview = { $type } files can't be shown here; they're shown once posted.
upload-unnamed = (no name)
upload-size = Size
upload-from = Uploaded from
upload-parent = Parent post
upload-post = Post
upload-post-anyway = Post anyway
upload-continue = Upload
upload-next-step = Next, each file gets its own page to tag and post it, showing it with posts that look like it.
upload-similar-title = { $count ->
    [one] This looks like a post already here
   *[other] This looks like { $count } posts already here
}
upload-similar-hint = Check it isn't one of these. If it isn't (a different version, an edit, another page), post it anyway; consider making it a child or parent of the post it resembles.
upload-drop-hint = Choose files, drop them onto this form, or paste them from your clipboard: up to { $max } at once. Choosing them uploads them.
upload-url = …or a link
upload-url-hint = Images or videos up to { $mb } MB. A work's page on Pixiv, X, Bluesky, DeviantArt, another booru or the many other sites Danbooru supports works too: its files are downloaded, and the page becomes the source. With files, the link is their source. Pasting a link anywhere on this page uploads it.
upload-supported-sites = See here for a list of supported sites.
upload-all-files = Download all { $count } files (at most { $max })
upload-bookmarklet-hint = The <a href="/uploads/bookmarklet">bookmarklet</a> uploads the page you're on in one click.
bookmarklet-title = Bookmarklet
bookmarklet-intro = A bookmarklet is a bookmark that does something on the page you're looking at. This one uploads it here: the work's files are found and downloaded, and you go straight to tagging them.
bookmarklet-link = Post to { $site }
bookmarklet-step-drag = Drag the button above to your bookmarks toolbar (or right-click it and bookmark it).
bookmarklet-step-open = Open a work's page on a site below, or an image on its own.
bookmarklet-step-click = Click the bookmark.
bookmarklet-personal = This bookmarklet is yours: it carries a key that lets it upload without asking first. Don't share it, or links others make could upload to your account as soon as you open them. The sites you use it on can see the key too: if you doubt one, make a new key and take the bookmarklet again.
bookmarklet-new-key = Make a new key
bookmarklet-new-key-confirm = Bookmarklets you took before will only fill in the upload form. Make a new key?
bookmarklet-log-in = Log in before taking the bookmarklet: one taken now only fills in the upload form, and you press Upload yourself.
bookmarklet-mobile = On a phone, copy the work's link and paste it on the upload page instead.
bookmarklet-sites-title = Supported sites
bookmarklet-sites-hint = Works on these sites are read for their files, artist, tags and commentary. Links to other pages work too: the page's preview image is used, or the link's file as it is.
upload-tags-hint = Separate tags with spaces. Give a new tag a category with a prefix, like <code>artist:name</code>. <code>-tag</code> and metatags like <code>rating:s</code> or <code>pool:name</code> work too.
commentary = Artist's commentary
upload-suggested-pending = The tagger is looking at this file. Its suggestions show here when it's done; you can post without waiting for them.
upload-suggested-check = Check again
upload-suggested-hint = Clicking one adds it to the form.
upload-commentary-hint = What the artist wrote with the work, as they wrote it, and an English translation. The original is filled in from the source when the site is one Moekura can read (Pixiv, X, Bluesky, DeviantArt and the other sites Danbooru supports, or a page with its image in its preview tags), and taken from there if left empty.
upload-download = Download
upload-for-approval = Upload for approval
upload-ctrl-enter = Ctrl+Enter posts it.
upload-divider = Width of the form
upload-dock = Form:
upload-dock-left = left
upload-dock-right = right
upload-dock-bottom = below
upload-help-title = Help
upload-help-new = Updated
upload-help-changed = This has changed since your last upload.
upload-help-page = The upload help page
upload-less-similar = { $count ->
    [one] Show 1 low similarity match
   *[other] Show { $count } low similarity matches
}
upload-related-title = Related posts
upload-related-found = { $count ->
    [one] Found <a href="{ $url }">1 other post</a> from the same source.
   *[other] Found <a href="{ $url }">{ $count } other posts</a> from the same source.
}
upload-source-fetch = Fetch source data
upload-source-site = Site
upload-source-artist = Artist
upload-source-new-artist = Create new artist
upload-source-published = Published
upload-source-updated = updated { $date }
upload-source-unread = Nothing could be read from this source.
upload-copy-id = Copy ID
upload-search-elsewhere = Search for it
upload-warnings = Warnings
upload-help = help
upload-no-source = No Source
upload-no-source-hint = Uploaded from a file: search SauceNAO for where it's from
upload-no-source-detail = If you can, upload the link to where you found the file instead.
upload-bad-source = Bad Source
upload-bad-source-detail = The source is the image itself: upload the page it's on instead.
upload-image-sample = Image Sample
upload-image-sample-detail = This is a resized copy: upload the full image instead.
upload-ai-generated = AI-Generated
upload-ai-generated-detail = The file's metadata says an image generator made it; check the rules on AI-generated images.
upload-pixel-duplicate = Pixel-Perfect Duplicate
upload-pixel-duplicate-one = It has exactly the pixels of <a href="/posts/{ $id }">post #{ $id }</a>.
upload-pixel-duplicate-many = It has exactly the pixels of <a href="{ $url }">{ $count } other posts</a>.

posts-newest = Newest posts
posts-syntax = See the <a href="https://docs.moekura.net/using/search.html">search syntax</a>.
tag-script = Tag script
post-mode = On click
post-mode-view = View the post
posts-save-search = Save this search
posts-labels-placeholder = e.g. artists
posts-your-saved = Your saved searches
posts-add-to-search = Add to search
posts-add-tag = Add { $tag } to search
posts-exclude = Exclude from search
posts-exclude-tag = Exclude { $tag } from search
posts-wiki-for = Wiki page for { $tag }
posts-read-wiki = Read the wiki page for { $title }
posts-blacklist-hidden = { $count } hidden by your blacklist (<a href="{ $url }">show</a>)
posts-blacklist-left-out = blacklisted posts left out (<a href="{ $url }">show</a>)
posts-blacklist-blurred = { $count } blurred by your blacklist (<a href="{ $url }">show</a>)
posts-deleted-hidden = { $count } hidden (<a href="{ $url }">show</a>)
posts-blacklist-showing = showing blacklisted posts (<a href="{ $url }">hide</a>)
posts-nothing-found = Nothing found
posts-no-match = No posts match <strong>{ $tags }</strong>.
posts-did-you-mean = Did you mean { $options }?
posts-did-you-mean-instead = Did you mean { $options } instead of { $term }?
posts-try-without = Try without the last term: <a href="{ $url }">{ $terms }</a>.
posts-look-for-tags = <a href="{ $url }">Look for tags starting with { $term }</a>, in case of a typo.
posts-none-yet = No posts yet
posts-end = That’s every post in this search.
posts-skip-tags = Skip the tags, to the posts
posts-none-yet-hint = Posts show up here as soon as they are uploaded and tagged.
posts-upload-first = Upload the first one

## The post page

# Headings in the classic layout's post sidebar, as on Danbooru.
post-information = Information
post-options = Options
post-id = ID
post-status = Status

post-is = This post is { $status }.
post-locked = Locked: { $locks }.
lock-rating = Rating
lock-tags = Tags
lock-notes = Notes
lock-status = Status
post-processing = Thumbnails are still being generated.
post-warnings = Worth a look before moving on:
tag-warning-no-artist = It has no artist tag: add an artist tag, or artist_unknown.
tag-warning-no-copyright = It has no copyright tag: add a copyright tag, or original.
tag-warning-no-character = It has no character tag: add a character tag, if anyone appears in it.
tag-warning-few-general = It has { $count } general { $count ->
    [one] tag
   *[other] tags
}; well-tagged posts have at least { $min }.
tag-warning-new-tags = No other post has these tags yet; check their spelling:
tag-warning-kept = { $name } is already { $vowel ->
    [yes] an
   *[no] a
} { $actual } tag, so { $wanted }: didn't change it; ask someone who manages tags if it's wrong.
post-pool = Pool { $name }
first = First
last = Last
post-pool-prev = ‹ prev
post-pool-next = next ›
post-blacklisted = This post matches your blacklist (<code>{ $rule }</code>). <a href="{ $url }">Show it anyway</a>
post-ugoira-preparing = This animation is still being prepared; it plays here once it's ready.
post-ugoira = An ugoira, played from its frames. <a href="{ $url }" download>Download the frames</a> (a zip).
post-resized-to = Resized to { $percent }% of the original.
post-notes = Notes ({ $count })
post-notes-hover = Show the notes' text when pointed at
post-notes-embed = Draw the notes' text on the picture
post-commentary-history = Commentary history
post-commentary-edit = Edit the commentary
post-commentary-add = Add the artist's commentary
post-commentary-original-title = Original title
post-commentary-original-description = Original description
post-commentary-translated-title = Translated title
post-commentary-translated-description = Translated description
post-commentary-hint = What the artist wrote where they posted this, as they wrote it, and a translation. Descriptions use the wiki's markup.
post-family = Related posts
post-has-parent = This post belongs to <a href="/posts/{ $id }">a parent post</a>.
post-has-children = This post has child posts.
post-similar = Similar posts
post-similar-search = (search)
post-disapprovals = Disapprovals
post-disapprovals-intro = Approvers passed on this post:
appeal = Appeal
post-appeal-why = Why should this post come back?
post-replace = Replace the file
post-replace-file = New file
post-replace-why = Why
post-replace-placeholder = Higher resolution, uncropped, …
post-replace-notes = Move and resize the notes to fit the new size
post-replace-hint = The post keeps its tags, comments, notes, pools and favourites; the old file is kept in its <a href="/posts/{ $id }/replacements">replacements</a>.
post-replace-confirm = Replace this post's file?
post-replace-button = Replace
flag = Flag
post-flag-why = What's wrong with this post?
post-flag = Flag for review
post-waiting = This post is waiting for approval.
reason-rejecting = Reason for rejecting
reason-deleting = Reason for deleting
post-locks = Locked against changes
post-locks-save = Save locks
post-purge-hint = Purging removes the post and its files for good.
post-purge-confirm = Purge post #{ $id }? It and its files are removed for good; this can't be undone.
post-tags-locked = The tags are locked.
post-parent = Parent post
post-copy-tags = Copy tags
post-copy-from-parent = From the parent #{ $id }
post-copy-from-child = From child #{ $id }
post-copy-hint = Clicking one adds that post's tags and saves the form.
post-suggested = Suggested by the tagger
post-suggested-pending = The tagger hasn't looked at this post yet. Its suggestions show here once it has, usually within a minute.
post-suggested-add = Add { $tag } ({ $percent }% sure)
post-suggested-rating = Rating:
post-rate-it = Rate it { $rating }
post-suggested-hint = Clicking one adds it and saves the form.
post-comments-hidden = Your settings hide comments. <a href="{ $url }">Read them</a>
post-older-comments = { $count } older { $count ->
    [one] comment
   *[other] comments
}
post-do-not-bump = Don't bump the post
post-log-in-to-comment = <a href="/login?next=%2Fposts%2F{ $id }">Log in</a> to comment.
post-search-results = Search results
post-previous-key = Previous (a)
post-next-key = Next (d)
post-all-posts = All posts
post-size = Size
post-no-sound = no sound
post-animated = animated
favorite = Favorite
post-uploaded = Uploaded
post-download = Download original
post-metadata = Metadata
post-replacements = Replacements
post-look-alikes = Look-alikes
post-note-history = Note history
post-remove-from = Remove from { $name }
remove-lower = remove
post-add-to-pool = Add to a pool
post-pool-field = Pool name or number

## The post page's action strip.
post-actions = Post actions
post-edit-tags = Edit tags
post-edit-panel = Edit tags, rating and source
post-more = More
post-more-hint = links: history, metadata, replacements, look-alikes
post-file = File
