# Artists

An artist tag can have an **artist entry**: the artist's other names,
their group or circle, and the places they post their work. Find them
under **Tags → Artists** (`/artists`), searchable by any of their names,
their group, or one of their URLs.

Anyone with the **Edit the wiki and artists** permission (members, by
default) can start or change an entry: **New artist** on the list, or
**Start an artist entry** on an artist tag's wiki page. An entry's name
is its tag; a tag that doesn't exist yet, or that no post uses yet,
becomes an artist tag. Every change is kept under **History**, and
**Recent changes** (`/artist_versions`) lists every entry's changes,
filterable by user. Entries can be deleted and restored; deleted ones,
and their changes, are left out for visitors and others without that
permission or *See deleted posts*.

The entry's page shows the URLs, the first paragraph of the tag's wiki
page (the longer description lives there), and the artist's newest
posts.

## URLs

List the artist's profiles and galleries, one per line. Put `-` in front
of ones no longer in use (a deleted account, a site that closed): they're
shown struck out, but still identify the artist. Profiles on the
[sites Moekura knows](sources.md) are saved in their canonical form
(`pixiv.net/member.php?id=1` becomes `https://www.pixiv.net/users/1`) and
shown with the site's icon.

**Find an artist by URL** (`/artists/finder`) takes any address, a
profile or a page of one of the artist's works, and lists the artists
whose URLs it falls under: `https://x.com/someone/status/123` finds the
artist with `https://twitter.com/someone`. Addresses are compared without
`www.`, the scheme, or anything after `?`, `x.com` counts as
`twitter.com`, and a profile's other forms count as its canonical one
(`someone.artstation.com` as `artstation.com/someone`).

The upload form does the same with the link you upload from and the
source: when they belong to a known artist, their tag is offered beside
the tags box. For works on the sites Moekura reads, the site is asked
who made it, so a work's page finds the artist whose entry lists their
profile, even when the page's address doesn't contain it (Pixiv's
don't); an artist without an entry gets a link to start one, with their
name and profiles filled in.

## Banned artists

Staff who can manage tags can ban an artist from their entry (for example
at the artist's request). What that does is a site setting, under
**Admin → Settings → Banned artists**:

- **Hide their posts** (on by default): posts with the artist's tag are
  left out of searches, and their pages aren't found.
- **Refuse uploads** (on by default): uploads with the tag, and edits
  adding it (reverting to a version that had it, too), are refused.

Staff who approve posts still see the posts and can post them. Bans are
recorded in the moderation log.

## Commentary

A post can have the **artist's commentary**: the title and description
the artist gave the work where they posted it, and a translation. It's
shown under the picture, translated when there's a translation (with the
original a click away), and anyone who can edit posts can add or change
it from the same place. Descriptions use the wiki's markup. Every change
is kept (**Commentary history**; `/artist_commentary_versions` lists
them sitewide), and `commentary:` [searches](search.md) find posts with
commentary, with or without a translation, or by its words:
`commentary:untranslated` lists commentary waiting for a translator.
