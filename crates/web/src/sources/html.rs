//! Reading what strategies need out of a site's HTML without a full
//! parser: `<meta>` tags, tags' attributes, an element's inner HTML, and
//! JSON embedded in scripts. Good enough for the pages sites serve; a
//! site that changes its markup just gives less.

use serde_json::Value;

use super::decode_entities;

/// An opening tag: its name (lowercase), attributes (names lowercase,
/// values decoded), and where it ends in the page.
#[derive(Debug, Clone)]
pub(crate) struct Tag {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    /// Just past the tag's `>`: where its content starts.
    pub end: usize,
}

impl Tag {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether its `class` lists `class`.
    pub fn has_class(&self, class: &str) -> bool {
        self.attr("class")
            .is_some_and(|c| c.split_whitespace().any(|c| c == class))
    }
}

/// Most tags [`tags`] gives: pages have far fewer of any one kind, and
/// strategies may look inside each.
const MAX_TAGS: usize = 5000;
/// How deeply elements of one kind may nest before [`inner`] stops
/// counting (and gives the rest of the page).
const MAX_DEPTH: usize = 256;
/// The longest a [`label`] may be, in bytes of HTML.
const MAX_LABEL: usize = 2048;
/// Elements that end where another of their kind starts, as browsers read
/// them: a page of unclosed ones then costs no more than closed ones.
const NO_NESTING: &[&str] = &["a", "p", "h1", "h2", "h3", "h4", "h5", "h6"];

/// Whether `html` has the tag `name` (any case) at `at`, the byte after
/// its `<` (or `</`): the name and then a space, `>` or `/`.
fn named_at(html: &[u8], at: usize, name: &[u8]) -> bool {
    html.get(at..at + name.len())
        .is_some_and(|n| n.eq_ignore_ascii_case(name))
        && html
            .get(at + name.len())
            .is_some_and(|&c| c.is_ascii_whitespace() || c == b'>' || c == b'/')
}

/// The opening `<name …>` tags in `html`, in order, read as they're asked
/// for (`name` lowercase).
fn opening<'a>(html: &'a str, name: &'a str) -> impl Iterator<Item = Tag> + 'a {
    let mut from = 0;
    std::iter::from_fn(move || {
        loop {
            let start = from + html.get(from..)?.find('<')?;
            // `<a` mustn't match `<abbr`.
            if !named_at(html.as_bytes(), start + 1, name.as_bytes()) {
                from = start + 1;
                continue;
            }
            let after = start + 1 + name.len();
            let end = tag_end(html, after)?;
            from = end;
            return Some(Tag {
                name: name.to_owned(),
                attrs: attributes(&html[after..end - 1]),
                end,
            });
        }
    })
}

/// Every opening `<name …>` tag in `html`, in order (`name` lowercase), up
/// to [`MAX_TAGS`] of them.
pub(crate) fn tags(html: &str, name: &str) -> Vec<Tag> {
    opening(html, name).take(MAX_TAGS).collect()
}

/// Where the tag whose attributes start at `from` ends (past its `>`),
/// skipping `>` inside quoted values.
fn tag_end(html: &str, from: usize) -> Option<usize> {
    let mut quote = None;
    for (i, c) in html[from..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(from + i + 1),
            _ => {}
        }
    }
    None
}

/// `a="1" b='2' c=3 d` as pairs.
fn attributes(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_ascii_lowercase();
        rest = rest[name_end..].trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (value, tail) = match after.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let inner = &after[1..];
                    let close = inner.find(q).unwrap_or(inner.len());
                    (&inner[..close], inner.get(close + 1..).unwrap_or(""))
                }
                _ => {
                    let close = after.find(char::is_whitespace).unwrap_or(after.len());
                    (&after[..close], &after[close..])
                }
            };
            rest = tail.trim_start();
            decode_entities(value)
        } else {
            rest = rest.trim_start_matches('/').trim_start();
            String::new()
        };
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
}

/// The HTML inside the element whose opening tag is `tag`, up to its
/// matching closing tag (nested ones of the same name counted), and
/// whether that was found within `limit` bytes (else it's the rest of the
/// page).
fn inner_within<'a>(html: &'a str, tag: &Tag, limit: usize) -> (&'a str, bool) {
    let bytes = html.as_bytes();
    let name = tag.name.as_bytes();
    let flat = NO_NESTING.contains(&tag.name.as_str());
    let stop = tag.end.saturating_add(limit).min(html.len());
    let mut depth = 1;
    let mut at = tag.end;
    // One pass from the tag on, never back.
    while let Some(i) = bytes
        .get(at..stop)
        .and_then(|rest| rest.iter().position(|&b| b == b'<'))
    {
        let start = at + i;
        let closing = bytes.get(start + 1) == Some(&b'/');
        if named_at(bytes, start + 1 + usize::from(closing), name) {
            if closing {
                depth -= 1;
                if depth == 0 {
                    return (&html[tag.end..start], true);
                }
            } else if flat {
                return (&html[tag.end..start], true);
            } else {
                depth += 1;
                if depth > MAX_DEPTH {
                    break;
                }
            }
        }
        at = start + 1;
    }
    (&html[tag.end..], false)
}

/// The HTML inside the element whose opening tag is `tag`, up to its
/// matching closing tag (nested ones of the same name counted).
pub(crate) fn inner<'a>(html: &'a str, tag: &Tag) -> &'a str {
    inner_within(html, tag, usize::MAX).0
}

/// The text of a short element (a tag's link, a name) when it ends within
/// [`MAX_LABEL`] bytes: for reading many of them from a page, which could
/// otherwise each be the rest of it.
pub(crate) fn label(html: &str, tag: &Tag) -> Option<String> {
    match inner_within(html, tag, MAX_LABEL) {
        (inner, true) => Some(super::html_to_text(inner)),
        (_, false) => None,
    }
}

/// The first element `<name>` for which `matches` holds, and its inner HTML.
pub(crate) fn find<'a>(
    html: &'a str,
    name: &str,
    matches: impl Fn(&Tag) -> bool,
) -> Option<(Tag, &'a str)> {
    let tag = opening(html, name).find(|t| matches(t))?;
    let inner = inner(html, &tag);
    Some((tag, inner))
}

/// The `content` of `<meta property|name|itemprop="…">` tags, by name
/// (lowercase), first wins.
pub(crate) fn meta(html: &str, name: &str) -> Option<String> {
    tags(html, "meta").into_iter().find_map(|t| {
        let key = t
            .attr("property")
            .or_else(|| t.attr("name"))
            .or_else(|| t.attr("itemprop"))?;
        (key.eq_ignore_ascii_case(name))
            .then(|| t.attr("content").map(str::to_owned))
            .flatten()
            .filter(|c| !c.is_empty())
    })
}

/// The page's `<title>`.
pub(crate) fn title(html: &str) -> Option<String> {
    find(html, "title", |_| true).map(|(_, inner)| decode_entities(inner.trim()))
}

/// The text between `start` and the next `end` after it.
pub(crate) fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let to = text[from..].find(end)? + from;
    Some(&text[from..to])
}

/// The JSON in `<script id="{id}">` (Next.js's `__NEXT_DATA__`, Nuxt's).
pub(crate) fn script_json(html: &str, id: &str) -> Option<Value> {
    let (_, inner) = find(html, "script", |t| t.attr("id") == Some(id))?;
    serde_json::from_str(inner.trim()).ok()
}

/// The JSON value (object or array) that starts after `marker`:
/// `window.__INITIAL_STATE__ = {…};` and the like.
pub(crate) fn json_after(text: &str, marker: &str) -> Option<Value> {
    let from = text.find(marker)? + marker.len();
    let start = from + text[from..].find(['{', '['])?;
    let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
    values.next()?.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tags_and_json() {
        let html = r#"<html><head><title>A &amp; B</title>
            <meta property="og:image" content="https://x/a.png">
            <meta name=description content='Hi "there"'></head>
            <body><div class="post big" data-id=3><div>in</div>text</div>
            <abbr>x</abbr><a href="/u/1">one</a>
            <script id="__NEXT_DATA__" type="application/json">{"a": [1, 2]}</script>
            <script>window.__STATE__ = {"b": {"c": "}"}};</script></body></html>"#;
        assert_eq!(title(html).as_deref(), Some("A & B"));
        assert_eq!(meta(html, "og:image").as_deref(), Some("https://x/a.png"));
        assert_eq!(meta(html, "description").as_deref(), Some("Hi \"there\""));
        let (div, content) = find(html, "div", |t| t.has_class("post")).unwrap();
        assert_eq!(div.attr("data-id"), Some("3"));
        assert_eq!(content, "<div>in</div>text");
        assert_eq!(tags(html, "a").len(), 1);
        assert_eq!(tags(html, "a")[0].attr("href"), Some("/u/1"));
        assert_eq!(script_json(html, "__NEXT_DATA__").unwrap()["a"][1], 2);
        assert_eq!(json_after(html, "__STATE__").unwrap()["b"]["c"], "}");
        assert_eq!(between(html, "<abbr>", "</abbr>"), Some("x"));
    }

    #[test]
    fn elements_end_where_browsers_end_them() {
        let html = r#"<DIV class=a><div>x</Div><abbr>y</abbr></div>after
            <a href=1>one<a href=2>two</a><p>p1<p>p2</p><span>open"#;
        let (_, inner) = find(html, "div", |t| t.has_class("a")).unwrap();
        assert_eq!(inner, "<div>x</Div><abbr>y</abbr>");
        let links = tags(html, "a");
        assert_eq!(links.len(), 2);
        assert_eq!(super::inner(html, &links[0]), "one");
        assert_eq!(super::inner(html, &links[1]), "two");
        assert_eq!(find(html, "p", |_| true).unwrap().1, "p1");
        // Unclosed: the rest of the page, but no label.
        let span = &tags(html, "span")[0];
        assert_eq!(super::inner(html, span), "open");
        assert_eq!(label(html, span), None);
        assert_eq!(label(html, &links[1]).as_deref(), Some("two"));
        let long = format!("<a>{}</a>", "x".repeat(MAX_LABEL));
        assert_eq!(label(&long, &tags(&long, "a")[0]), None);
    }

    #[test]
    fn large_pages_read_in_one_pass() {
        // Nested elements of one kind, never closed.
        let nested = "<div class=x>".repeat(200_000);
        // Many tag links, never closed either, and spans like them.
        let links = "<a rel=tag>cat".repeat(200_000);
        let spans = "<span class=hashtag>#cat".repeat(200_000);
        let started = std::time::Instant::now();
        let (_, inner) = find(&nested, "div", |_| true).unwrap();
        assert_eq!(inner.len(), nested.len() - "<div class=x>".len());
        assert!(find(&nested, "div", |t| t.has_class("y")).is_none());
        let found = tags(&links, "a");
        assert_eq!(found.len(), MAX_TAGS);
        let names: Vec<String> = found.iter().filter_map(|a| label(&links, a)).collect();
        assert_eq!(names.len(), MAX_TAGS);
        assert!(names.iter().all(|n| n == "cat"));
        let hashtags = tags(&spans, "span")
            .iter()
            .filter_map(|s| label(&spans, s))
            .count();
        assert_eq!(hashtags, 0);
        assert!(meta(&links, "og:image").is_none());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }
}
