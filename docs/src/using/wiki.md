# The wiki

Every tag can have a wiki page explaining what it's for and how to use
it. Find them under **Tags → Wiki**, or with the **?** next to a tag in
search results and on post pages. Searching for a single tag shows the
first paragraph of its page above the results.

A page's title is the tag's name, so `Long Hair` and `long_hair` are the
same page, and a page can exist before any post has the tag. The page of
an aliased tag points to the tag it's aliased to.

Anyone with the **Edit the wiki** permission (members, by default) can
start or change a page. Every change is kept: **History** lists them,
and you can view any old version or revert to it. If someone else saves
the page while you're editing it, you're told instead of overwriting
their change, and your text is kept so you can save again.

## Formatting

| Write | For |
|---|---|
| A blank line | A new paragraph |
| `h2. Heading` (`h1.` to `h6.`) | A heading, at the start of a line |
| `* item`, `** nested item` | A list |
| `[b]bold[/b]`, `[i]italic[/i]`, `[s]struck[/s]`, `[u]underlined[/u]` | Styles |
| `[[long_hair]]`, `[[long_hair\|long hair]]` | A link to a wiki page, with its own text after the `\|` |
| `{{cat -dog}}` | A link to search results |
| `post #123` | A link to a post |
| `https://example.com` | A link to another site |

Everything else is shown as written; HTML isn't allowed. The syntax is a
subset of Danbooru's DText, so pages copied from there mostly work.
