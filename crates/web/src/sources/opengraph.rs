//! Any other page: its OpenGraph (and Twitter card) tags name the image,
//! the title and a description. Pages without an image give nothing.

use url::Url;

use super::{Http, MAX_URL, SourceInfo, decode_entities};

/// The longest `content` read, in bytes: a title or a description is far
/// shorter.
const MAX_CONTENT: usize = 64 * 1024;

/// The `content` of `<meta property|name="…">` tags, by name, first wins.
/// Longer ones than [`MAX_CONTENT`] are left out.
fn meta_tags(html: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(start) = lower[from..].find("<meta") {
        let start = from + start;
        let Some(end) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..start + end];
        let attr = |name: &str| {
            let lower_tag = tag.to_ascii_lowercase();
            let at = lower_tag.find(&format!("{name}="))? + name.len() + 1;
            let rest = &tag[at..];
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let rest = &rest[1..];
            let value = &rest[..rest.find(quote)?];
            (value.len() <= MAX_CONTENT).then(|| decode_entities(value))
        };
        if let (Some(key), Some(content)) =
            (attr("property").or_else(|| attr("name")), attr("content"))
        {
            found.push((key.to_ascii_lowercase(), content));
        }
        from = start + end;
    }
    found
}

pub(super) fn parse(html: &str, page: &Url) -> Option<SourceInfo> {
    let tags = meta_tags(html);
    let get = |names: &[&str]| {
        names.iter().find_map(|name| {
            tags.iter()
                .find(|(k, v)| k == name && !v.is_empty())
                .map(|(_, v)| v.clone())
        })
    };
    let image = get(&["og:image:secure_url", "og:image", "twitter:image"])?;
    if image.len() > MAX_URL {
        return None;
    }
    let image = page.join(&image).ok()?;
    if !matches!(image.scheme(), "http" | "https") {
        return None;
    }
    Some(SourceInfo {
        site: "the page",
        page_url: page.to_string(),
        files: vec![image.to_string()],
        headers: vec![("Referer", page.to_string())],
        artist_name: None,
        artist_account: None,
        profile_urls: Vec::new(),
        tags: Vec::new(),
        title: get(&["og:title", "twitter:title"]).unwrap_or_default(),
        description: get(&["og:description", "twitter:description", "description"])
            .unwrap_or_default(),
        ugoira_frames: None,
        published_at: get(&["article:published_time"]).and_then(|d| super::date(&d)),
        updated_at: get(&["article:modified_time", "og:updated_time"])
            .and_then(|d| super::date(&d)),
    })
}

/// What a page says, read without a login: its address is a link as it
/// was given, or a known site's page made from one (which a strategy
/// may have refused as crafted), so it could be any page on the site.
pub(super) async fn fetch(http: &Http<'_>, url: &Url) -> Result<Option<SourceInfo>, String> {
    let (content_type, body) = http.text_without_login(url.as_str(), &[]).await?;
    if !content_type.starts_with("text/html") {
        return Ok(None);
    }
    Ok(parse(&body, url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_contents_are_left_out() {
        let page = Url::parse("https://example.com/work").unwrap();
        // `&`s without a `;` near them once took minutes to decode.
        let long = "&".repeat(MAX_CONTENT + 1);
        let html = format!(
            r#"<meta property="og:image" content="/a.png">
            <meta name="description" content="{long}">
            <meta property="og:title" content="A &amp; B">"#
        );
        let info = parse(&html, &page).unwrap();
        assert_eq!(info.files, ["https://example.com/a.png"]);
        assert_eq!(info.title, "A & B");
        assert_eq!(info.description, "");
        // An image address that long isn't kept.
        let html = format!(
            r#"<meta property="og:image" content="/{}">"#,
            "a".repeat(MAX_URL)
        );
        assert_eq!(parse(&html, &page), None);
    }
}
