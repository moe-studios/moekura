# Introduction

Moekura is a self-hostable booru: an image board where posts are found by
their tags. The same program and database layout serve a private,
single-user collection on a small VPS or Raspberry Pi and a public site with
millions of posts, many web servers and a CDN. Growing a site means changing
configuration and running more processes, never switching software.

What it does:

- **Posts**: upload images (JPEG, PNG, GIF, WebP, AVIF, optionally JPEG XL)
  and videos (MP4, WebM) from a file or a link. Exact duplicates are
  refused; similar images are found by their perceptual hash.
- **Tags**: categories (general, artist, copyright, character, meta),
  aliases and implications, autocomplete, and a [search
  syntax](using/search.md) with tags, wildcards and filters. An optional
  [tagger](admin/tagger.md) suggests tags for new uploads with a machine
  learning model, on the CPU.
- **People**: accounts with roles, favorites, votes, blacklists and
  per-user settings; registration can be open, invite-only, approved by
  staff, or closed.
- **Moderation**: an approval queue, flags, deletion with reasons, bans of
  users and networks, and a log of every staff action.
- **Private sites**: keep everything behind a login, with file links that
  expire.
- **An API** covering what the site does, with API keys and a reference
  generated from the code.

It is free software under the [AGPL-3.0](https://www.gnu.org/licenses/agpl-3.0.html):
if you run a modified version as a public service, you must publish your
changes.

Start with [installing it with Docker Compose](install/compose.md).
