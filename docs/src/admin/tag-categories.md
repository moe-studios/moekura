# Tag categories

Tags belong to a category, which decides how post pages and tag lists
group and colour them. Every site starts with Danbooru's five: general,
artist, copyright, character and meta. Admins (anyone who can manage site
settings) change them under **Admin → Tag categories**:

- **Relabel** any category: the label is what people see, such as
  "Creator" for artist.
- **Reorder** them with *Move up* and *Move down*; tag lists show the
  groups in this order.
- **Add** a category with a name and a label, e.g. `species` / "Species".
  The name is how people put a tag in it (`species:cat` in a tag box) and
  search by it (`speciestags:2`). It must be lowercase letters, digits and
  underscores, can't be a word searches already use (`rating`, `gen`, …),
  and can't be how existing tag names start (rename `species:…` tags
  first).
- **Rename** a category you added. Its tags stay in it.
- **Delete** a category you added once no tag is in it; move its tags
  elsewhere first (from the tag's page, or with a mass edit
  `category tag -> general`).

Every change is recorded in the moderation log.

## What can't change

- **Ids.** Each category keeps its number for good, and a deleted
  category's number isn't given to a new one if tag history still
  mentions it. Posts' per-category tag counts, tag history and the
  Danbooru-compatible API (`tag_category`, `category` in `/tags.json`)
  refer to categories by id.
- **Danbooru's names and ids** (general 0, artist 1, copyright 3,
  character 4, meta 5). Danbooru apps, imports from other boorus and the
  tagger rely on them, so these can only be relabelled and reordered, not
  renamed or deleted. Categories you add get ids from 6 up.

Changes apply on every server at once: categories are read from the
database, not cached.

## Colours

Tags in a category get the `.tag-<name>` class. Danbooru's categories
are coloured by the theme's `--tag-<name>` variables; a
[theme](themes.md) can colour a new one:

```css
.tag-species { color: light-dark(#00707a, #4fd1d9); }
```

Without that, its tags take general's colour.
