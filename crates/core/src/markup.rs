//! Wiki markup: a small subset of Danbooru's DText, rendered to HTML.
//!
//! - Paragraphs are separated by a blank line; a single line break stays
//!   a line break.
//! - `h1.` to `h6.` at the start of a line make a heading.
//! - Lines starting with `* ` (`** ` and so on to nest) make a list.
//! - `[b]`, `[i]`, `[s]` and `[u]` for bold, italic, struck and
//!   underlined text.
//! - `[[tag]]` or `[[tag|text]]` links to a tag's wiki page, `{{search}}`
//!   to search results, and `post #123` to a post.
//! - `http://` and `https://` URLs become links.
//!
//! The output is built from escaped text and a fixed set of elements, so
//! it needs no sanitising: nothing in the input becomes HTML unless the
//! renderer put it there.

use std::fmt::Write;

/// Longest wiki text, in bytes.
pub const MAX_LEN: usize = 50_000;

/// Longest [`excerpt`], in characters, before it's cut short.
const EXCERPT_LEN: usize = 500;

/// The wiki page about `title`.
pub fn wiki_url(title: &str) -> String {
    format!("/wiki/{}", encode(&crate::tags::normalize(title)))
}

/// Renders `text` to HTML.
pub fn render(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut paragraph: Vec<&str> = Vec::new();
    let mut list_depth = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            flush_paragraph(&mut out, &mut paragraph);
            close_list(&mut out, &mut list_depth, 0);
        } else if let Some((level, rest)) = heading(line) {
            flush_paragraph(&mut out, &mut paragraph);
            close_list(&mut out, &mut list_depth, 0);
            // `h1.` is an h2: the page title is the h1.
            let level = (level + 1).min(6);
            let _ = write!(out, "<h{level}>");
            inline(&mut out, rest);
            let _ = write!(out, "</h{level}>");
        } else if let Some((depth, rest)) = list_item(line) {
            flush_paragraph(&mut out, &mut paragraph);
            if depth > list_depth {
                for _ in list_depth..depth {
                    out.push_str("<ul><li>");
                }
            } else {
                close_list(&mut out, &mut list_depth, depth);
                out.push_str("</li><li>");
            }
            list_depth = depth;
            inline(&mut out, rest);
        } else {
            close_list(&mut out, &mut list_depth, 0);
            paragraph.push(line);
        }
    }
    flush_paragraph(&mut out, &mut paragraph);
    close_list(&mut out, &mut list_depth, 0);
    out
}

/// The first paragraph of `text`, skipping headings, cut to about 500
/// characters, for showing above search results. Empty if there's none.
pub fn excerpt(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines().map(str::trim_end) {
        let other_block = line.is_empty() || heading(line).is_some() || list_item(line).is_some();
        match (other_block, lines.is_empty()) {
            (true, true) => continue,
            (true, false) => break,
            (false, _) => lines.push(line),
        }
    }
    let paragraph = lines.join("\n");
    if paragraph.chars().count() <= EXCERPT_LEN {
        return paragraph;
    }
    let cut = paragraph
        .char_indices()
        .nth(EXCERPT_LEN)
        .map_or(paragraph.len(), |(i, _)| i);
    // At a word break, so a link isn't cut in half where possible.
    let end = paragraph[..cut].rfind(char::is_whitespace).unwrap_or(cut);
    format!("{}…", paragraph[..end].trim_end())
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let rest = line.strip_prefix('h')?;
    let level = rest.chars().next()?.to_digit(10)? as usize;
    let rest = rest[1..].strip_prefix(". ")?;
    (1..=6)
        .contains(&level)
        .then_some((level, rest.trim_start()))
}

fn list_item(line: &str) -> Option<(usize, &str)> {
    let depth = line.bytes().take_while(|&b| b == b'*').count();
    let rest = line[depth..].strip_prefix(' ')?;
    (depth > 0).then_some((depth, rest.trim_start()))
}

fn flush_paragraph(out: &mut String, lines: &mut Vec<&str>) {
    if lines.is_empty() {
        return;
    }
    out.push_str("<p>");
    inline(out, &lines.join("\n"));
    out.push_str("</p>");
    lines.clear();
}

/// Closes open lists until `depth` are left open.
fn close_list(out: &mut String, open: &mut usize, depth: usize) {
    while *open > depth {
        out.push_str("</li></ul>");
        *open -= 1;
    }
}

const STYLES: [(&str, &str); 4] = [("b", "strong"), ("i", "em"), ("s", "s"), ("u", "u")];

/// Renders the markup within a block.
fn inline(out: &mut String, text: &str) {
    let mut open: Vec<&str> = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let at_word_start = text[..text.len() - rest.len()]
            .chars()
            .next_back()
            .is_none_or(|p| !p.is_alphanumeric());
        if let Some(used) = link(out, rest, "[[", "]]", |target| {
            (!crate::tags::normalize(target).is_empty()).then(|| wiki_url(target))
        }) {
            rest = &rest[used..];
        } else if let Some(used) = link(out, rest, "{{", "}}", |query| {
            let query = query.split_whitespace().collect::<Vec<_>>().join(" ");
            (!query.is_empty()).then(|| format!("/posts?tags={}", encode(&query)))
        }) {
            rest = &rest[used..];
        } else if let Some((tag, element, used)) = style_tag(rest, false) {
            open.push(tag);
            let _ = write!(out, "<{element}>");
            rest = &rest[used..];
        } else if let Some((_, element, used)) =
            style_tag(rest, true).filter(|(tag, ..)| open.last() == Some(tag))
        {
            open.pop();
            let _ = write!(out, "</{element}>");
            rest = &rest[used..];
        } else if let Some(used) = at_word_start.then(|| post_link(out, rest)).flatten() {
            rest = &rest[used..];
        } else if let Some(used) = at_word_start.then(|| url_link(out, rest)).flatten() {
            rest = &rest[used..];
        } else {
            if c == '\n' {
                out.push_str("<br>");
            } else {
                escape_char(out, c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    for tag in open.into_iter().rev() {
        let element = STYLES
            .iter()
            .find(|(t, _)| *t == tag)
            .map_or("span", |s| s.1);
        let _ = write!(out, "</{element}>");
    }
}

/// `[b]` (or `[/b]` when `closing`) at the start of `text`: the tag, its
/// element and the bytes it takes up.
fn style_tag(text: &str, closing: bool) -> Option<(&'static str, &'static str, usize)> {
    let rest = text.strip_prefix('[')?;
    let rest = if closing {
        rest.strip_prefix('/')?
    } else {
        rest
    };
    STYLES.iter().find_map(|&(tag, element)| {
        rest.strip_prefix(tag)?.strip_prefix(']')?;
        Some((tag, element, tag.len() + 2 + usize::from(closing)))
    })
}

/// A `[[target|label]]`-style link at the start of `text`, written to
/// `out`; returns the bytes used. `href` makes the URL from the target.
fn link(
    out: &mut String,
    text: &str,
    open: &str,
    close: &str,
    href: impl Fn(&str) -> Option<String>,
) -> Option<usize> {
    let inner_start = open.len();
    let body = text.strip_prefix(open)?;
    let end = body.find(close)?;
    let inner = &body[..end];
    if inner.contains('\n') || inner.contains(open) {
        return None;
    }
    let (target, label) = match inner.split_once('|') {
        Some((target, label)) if !label.trim().is_empty() => (target, label.trim()),
        Some((target, _)) => (target, target.trim()),
        None => (inner, inner.trim()),
    };
    let url = href(target)?;
    let class = if open == "[[" {
        "wiki-link"
    } else {
        "search-link"
    };
    let _ = write!(out, "<a class=\"{class}\" href=\"");
    escape(out, &url);
    out.push_str("\">");
    escape(out, label);
    out.push_str("</a>");
    Some(inner_start + end + close.len())
}

/// `post #123` at the start of `text`.
fn post_link(out: &mut String, text: &str) -> Option<usize> {
    let rest = text
        .strip_prefix("post #")
        .or_else(|| text.strip_prefix("Post #"))?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let id: i64 = rest[..digits].parse().ok()?;
    let used = text.len() - rest.len() + digits;
    let _ = write!(out, "<a href=\"/posts/{id}\">");
    escape(out, &text[..used]);
    out.push_str("</a>");
    Some(used)
}

/// A bare `http(s)://` URL at the start of `text`.
fn url_link(out: &mut String, text: &str) -> Option<usize> {
    if !(text.starts_with("http://") || text.starts_with("https://")) {
        return None;
    }
    let mut end = text
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '[' | ']'))
        .unwrap_or(text.len());
    // Punctuation after a URL is usually the sentence's, and a `)` only
    // belongs to the URL if it opened one.
    loop {
        let candidate = &text[..end];
        let Some(last) = candidate.chars().next_back() else {
            break;
        };
        let unbalanced =
            last == ')' && candidate.matches('(').count() < candidate.matches(')').count();
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'') || unbalanced {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    let url = url::Url::parse(&text[..end]).ok()?;
    url.host_str()?;
    out.push_str("<a rel=\"nofollow ugc noopener\" href=\"");
    escape(out, url.as_str());
    out.push_str("\">");
    escape(out, &text[..end]);
    out.push_str("</a>");
    Some(end)
}

/// Percent-encodes `text` for a path segment or query value.
fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        escape_char(out, c);
    }
}

fn escape_char(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#x27;"),
        c => out.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_and_line_breaks() {
        assert_eq!(render(""), "");
        assert_eq!(
            render("one\ntwo\r\n\n\nthree  "),
            "<p>one<br>two</p><p>three</p>"
        );
    }

    #[test]
    fn escapes_everything_else() {
        assert_eq!(
            render("<script>alert('x' & \"y\")</script>"),
            "<p>&lt;script&gt;alert(&#x27;x&#x27; &amp; &quot;y&quot;)&lt;/script&gt;</p>"
        );
        assert_eq!(
            render("[[a\"><script>|<b>]]"),
            "<p><a class=\"wiki-link\" href=\"/wiki/a%22%3E%3Cscript%3E\">&lt;b&gt;</a></p>"
        );
    }

    #[test]
    fn headings() {
        assert_eq!(
            render("h1. Title\ntext\nh4.   Deeper\nh7. no\nh2.no"),
            "<h2>Title</h2><p>text</p><h5>Deeper</h5><p>h7. no<br>h2.no</p>"
        );
        assert_eq!(render("h6. Six"), "<h6>Six</h6>");
    }

    #[test]
    fn lists() {
        assert_eq!(
            render("Intro\n* one\n* two\n** nested\n* three\n\nafter"),
            "<p>Intro</p><ul><li>one</li><li>two<ul><li>nested</li></ul></li><li>three</li></ul><p>after</p>"
        );
        assert_eq!(render("*not a list*"), "<p>*not a list*</p>");
        assert_eq!(
            render("** deep"),
            "<ul><li><ul><li>deep</li></ul></li></ul>"
        );
    }

    #[test]
    fn styles_nest_and_close() {
        assert_eq!(
            render("[b]bold [i]both[/i][/b] [s]gone[/s] [u]under"),
            "<p><strong>bold <em>both</em></strong> <s>gone</s> <u>under</u></p>"
        );
        // A closing tag that doesn't match the innermost open one is text.
        assert_eq!(
            render("[b][i]x[/b]"),
            "<p><strong><em>x[/b]</em></strong></p>"
        );
        assert_eq!(render("[/i] [x]"), "<p>[/i] [x]</p>");
    }

    #[test]
    fn wiki_and_search_links() {
        assert_eq!(
            render("See [[Long Hair]], [[fate/stay_night|the series]] and [[ ]]."),
            "<p>See <a class=\"wiki-link\" href=\"/wiki/long_hair\">Long Hair</a>, \
             <a class=\"wiki-link\" href=\"/wiki/fate%2Fstay_night\">the series</a> and [[ ]].</p>"
        );
        assert_eq!(
            render("{{cat  -dog}} and {{solo|just one}}"),
            "<p><a class=\"search-link\" href=\"/posts?tags=cat+-dog\">cat  -dog</a> and \
             <a class=\"search-link\" href=\"/posts?tags=solo\">just one</a></p>"
        );
        assert_eq!(render("[[split\nline]]"), "<p>[[split<br>line]]</p>");
    }

    #[test]
    fn post_links() {
        assert_eq!(
            render("post #12, Post #3 but not repost #4 or post #x"),
            "<p><a href=\"/posts/12\">post #12</a>, <a href=\"/posts/3\">Post #3</a> \
             but not repost #4 or post #x</p>"
        );
    }

    #[test]
    fn bare_urls() {
        assert_eq!(
            render("Visit https://example.com/a_(b)?q=1&r=2. (http://x.org/path)"),
            "<p>Visit <a rel=\"nofollow ugc noopener\" href=\"https://example.com/a_(b)?q=1&amp;r=2\">\
             https://example.com/a_(b)?q=1&amp;r=2</a>. (<a rel=\"nofollow ugc noopener\" \
             href=\"http://x.org/path\">http://x.org/path</a>)</p>"
        );
        assert_eq!(render("javascript:alert(1)"), "<p>javascript:alert(1)</p>");
        assert_eq!(render("https://"), "<p>https://</p>");
        assert_eq!(render("xhttps://a.b"), "<p>xhttps://a.b</p>");
    }

    #[test]
    fn wiki_urls() {
        assert_eq!(wiki_url("Fate/Stay Night"), "/wiki/fate%2Fstay_night");
    }

    #[test]
    fn excerpts() {
        assert_eq!(
            excerpt("h1. About\n\nFirst [[line]]\nsecond\n\nNext paragraph"),
            "First [[line]]\nsecond"
        );
        assert_eq!(excerpt("* only\n* a list"), "");
        assert_eq!(excerpt("Text\n* list"), "Text");
        let long = "word ".repeat(200);
        let cut = excerpt(&long);
        assert!(cut.ends_with("word…"), "{cut}");
        assert!(cut.chars().count() <= EXCERPT_LEN + 1);
    }
}
