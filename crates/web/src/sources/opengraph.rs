//! Any other page: its OpenGraph (and Twitter card) tags name the image,
//! the title and a description. Pages without an image give nothing.

use url::Url;

use super::{Http, SourceInfo, decode_entities};

/// The `content` of `<meta property|name="…">` tags, by name, first wins.
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
            Some(decode_entities(&rest[..rest.find(quote)?]))
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
    })
}

pub(super) async fn fetch(http: &Http<'_>, url: &Url) -> Result<Option<SourceInfo>, String> {
    let (content_type, body) = http.text(url.as_str(), &[]).await?;
    if !content_type.starts_with("text/html") {
        return Ok(None);
    }
    Ok(parse(&body, url))
}
