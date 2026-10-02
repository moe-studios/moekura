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

/// Every opening `<name …>` tag in `html`, in order.
pub(crate) fn tags(html: &str, name: &str) -> Vec<Tag> {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{name}");
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find(&open) {
        let start = from + at;
        let after = start + open.len();
        // `<a` mustn't match `<abbr`.
        if !lower[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == '>' || c == '/')
        {
            from = after;
            continue;
        }
        let Some(end) = tag_end(html, after) else {
            break;
        };
        found.push(Tag {
            name: name.to_owned(),
            attrs: attributes(&html[after..end - 1]),
            end,
        });
        from = end;
    }
    found
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
/// matching closing tag (nested ones of the same name counted).
pub(crate) fn inner<'a>(html: &'a str, tag: &Tag) -> &'a str {
    let lower = html.to_ascii_lowercase();
    let (open, close) = (format!("<{}", tag.name), format!("</{}", tag.name));
    let mut depth = 1;
    let mut at = tag.end;
    while depth > 0 {
        let next_close = lower[at..].find(&close).map(|i| at + i);
        let next_open = lower[at..].find(&open).map(|i| at + i);
        match (next_open, next_close) {
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                at = o + open.len();
            }
            (_, Some(c)) => {
                depth -= 1;
                if depth == 0 {
                    return &html[tag.end..c];
                }
                at = c + close.len();
            }
            _ => return &html[tag.end..],
        }
    }
    &html[tag.end..]
}

/// The first element `<name>` for which `matches` holds, and its inner HTML.
pub(crate) fn find<'a>(
    html: &'a str,
    name: &str,
    matches: impl Fn(&Tag) -> bool,
) -> Option<(Tag, &'a str)> {
    let tag = tags(html, name).into_iter().find(|t| matches(t))?;
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
}
