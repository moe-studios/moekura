# Themes and layouts

## Layouts

Moekura has two layouts, and every theme works with both:

| Layout | Looks like |
|---|---|
| Modern | Moekura's own: a header band with the search and an account menu, and the image first on post pages |
| Classic | Danbooru: tabs along the top with a strip of the current section's links under them, and the search, tags and post details in a left sidebar |

Choose the layout visitors see under **Admin → Settings → Default
layout**. Logged-in users can choose the other under **Settings**, or
switch from the account menu (Modern) or the foot of any page (Classic).

The classic layout has its own versions of a few templates (the page
around everything, the post grid and post page, thumbnails and profiles)
in `classic/`; every other page is shared. To replace one of them, put it
in `classic/` in your
[`templates_override`](../configuration.md#paths) directory.

## Themes

A theme sets the site's colours, in both light and dark mode. Moekura
comes with five:

| Theme | Colours |
|---|---|
| Default | cool drawing-paper white, graphite ink and a deep cyan header |
| Forest | moss green |
| Neutral | greys, near-black ink and a charcoal header |
| Ocean | sea-foam links under a night-sea navy header |
| Sakura | pink under a plum header, Moekura's colours before 0.5 |

Choose the theme visitors see under **Admin → Settings → Default
theme**. Logged-in users can pick any theme the site has, and light,
dark or their device’s mode, under **Settings** or in the account menu of any
page. A user whose theme is removed gets the site's default again.

## Adding a theme

A theme is a stylesheet named `themes/<name>.css` in the
[`static_override`](../configuration.md#paths) directory. The name may
use lowercase letters, digits and dashes (`high-contrast` shows as
“High contrast”), but not `system`, `light` or `dark`. The file sets the
colour tokens of `css/main.css`, each as `light-dark(<light>, <dark>)`;
tokens it leaves out keep the default theme's colours:

```css
/* themes/dusk.css */
:root {
  --bg: light-dark(#faf7f2, #17140f);
  --surface: light-dark(#ffffff, #201c16);
  --text: light-dark(#231f19, #efe9df);
  --muted: light-dark(#6b6358, #a9a093);
  --border: light-dark(#e7e0d4, #37302a);
  --accent: light-dark(#a1471a, #f0925c);
  --accent-text: light-dark(#ffffff, #17140f);
  --danger: light-dark(#c92a2a, #ff8787);
  /* Tag categories. */
  --tag-general: light-dark(#0068e0, #4fa3ff);
  --tag-artist: light-dark(#c00004, #ff8a8b);
  --tag-copyright: light-dark(#a800aa, #d98cff);
  --tag-character: light-dark(#007e25, #35c64a);
  --tag-meta: light-dark(#ad5c00, #ffb54a);
}
```

A few more tokens are made from those, so a theme that sets only the
colours above still gets a header and ruling to match. Set them too to
choose them yourself:

| Token | What it colours |
|---|---|
| `--shell`, `--shell-text` | the header band and its text (keep 4.5:1 between them) |
| `--rule` | hairlines between table rows and facts |
| `--tint` | hovered and highlighted rows |
| `--staff`, `--staff-text` | the board under staff tools on post pages |

The classic layout uses `css/classic.css` and reads the same tokens,
so a theme written for the modern layout colours it too. It has a few of
its own: `--accent-hover` (links under the pointer), `--brand` (the site's
name), `--menu` (the current tab and the strip under the tabs),
`--stripe` (every other table row) and `--notice` and `--notice-border`
(messages). To give the classic layout different colours, add a block
for it to the theme's file:

```css
:root[data-layout="classic"] {
  --bg: light-dark(#ffffff, #17140f);
  --menu: light-dark(#f6ece0, #2b241c);
}
```

Keep text, links (`--accent`) and tag colours at a contrast of at least
4.5:1 against `--bg` and `--surface` (and `--menu`, `--stripe` and
`--tint` in the classic layout), and `--accent-text` against `--accent`,
so everyone can read them. Restart `moekura serve` to pick
up new or changed themes. A file with a built-in theme's name replaces
it.
