# Translations

Pages are shown in each reader's language: the one they chose under
**Settings → Language**, or else the best match for their browser's
languages, or else English. The language menu only appears once the site
has more than one language.

Moekura ships in English (`en-US`). Its messages are
[Fluent](https://projectfluent.org/) files in
[`crates/web/locales/en-US`](https://github.com/moe-studios/moekura/tree/main/crates/web/locales/en-US),
one per area of the site (`posts.ftl`, `moderation.ftl` and so on).

## Adding a language

Point `paths.locales_override` at a directory, and give it a
subdirectory per language, named by its tag (`de`, `pt-BR`), holding
`.ftl` files:

```toml
[paths]
locales_override = "/srv/moekura/locales"
```

```
/srv/moekura/locales/
  de/
    common.ftl
    posts.ftl
```

Copy the English files and translate their messages; keep each key
(left of the `=`) as it is. Messages you leave out show in English, so a
language can be translated a part at a time. Each language needs a
`language-name` message, its name in itself, for the menu:

```ftl
language-name = Deutsch
nav-posts = Beiträge
```

Messages are HTML: keep the tags the English ones have, and write `&lt;`
for a literal `<`. Variables like `{ $count }` are filled in by the page;
use [Fluent selectors](https://projectfluent.org/fluent/guide/selectors.html)
for plurals and other forms. A message with a `{$name}` (no spaces) in
`scripts.ftl` is filled in by the page's scripts, so these can't use
selectors.

Files for `en-US` in the directory replace the built-in messages they
define, which is how to reword the English ones. Restart after changing
the files; a file that doesn't parse stops startup with the reason.

## What isn't translated

Text people wrote (posts, tags, the wiki, comments, the rules), tag
category names, role names and forum categories show as written.
Emails, feeds, the APIs (including the API reference's descriptions) and
the error messages specific forms give are in English. Notifications and
spam-filter reasons are worded when they happen, so their details keep
the language they were written in.
