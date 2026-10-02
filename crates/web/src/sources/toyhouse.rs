//! Toyhouse images, from the image's page: the file, the artist credited
//! (often on another site) and the characters.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    // The image's id: …/~images/<id>, or the last number in the path.
    let id = page
        .rsplit(['/', '#'])
        .find(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .ok_or("Toyhouse: not an image")?;
    let body = http
        .page(&format!("https://toyhou.se/~images/{id}"), &[])
        .await?;
    image_info(known, page, &body).ok_or_else(|| "Toyhouse: no image in the page".into())
}

fn image_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let (_, content) = html::find(body, "div", |t| t.attr("id") == Some("content"))?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(content, "img")
        .into_iter()
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .take(1)
        .collect();
    if let Some((_, credit)) = html::find(body, "div", |t| t.has_class("artist-credit"))
        && let Some((link, name)) = html::find(credit, "a", |_| true)
    {
        let name = html_to_text(name);
        if let Some(href) = link.attr("href") {
            info.profile_urls.push(
                moekura_core::sites::parse(href)
                    .and_then(|u| u.profile_url)
                    .unwrap_or_else(|| href.to_owned()),
            );
        }
        if !name.starts_with("http") {
            info.artist_name = Some(name);
        }
    }
    info.tags = tags_named(
        html::tags(body, "a")
            .into_iter()
            .filter(|a| a.has_class("character-name-badge"))
            .map(|a| {
                let name = html_to_text(html::inner(body, &a));
                name.split(" (").next().unwrap_or(&name).to_owned()
            })
            .collect::<Vec<_>>(),
    );
    info.description = html::find(body, "div", |t| t.has_class("image-description"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images() {
        let page = "https://toyhou.se/~images/58037599";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div id="content"><img src="https://f2.toyhou.se/file/f2-toyhou-se/images/58037599_Ov5j4w66lQRw9G4.png"></div>
            <div class="image-credits"><div class="artist-credit"><a href="https://x.com/someone">someone</a></div></div>
            <div class="image-characters"><a class="character-name-badge" href="/1.june">June (art)</a></div>"#;
        let info = image_info(&known, page, body).unwrap();
        assert_eq!(info.files.len(), 1);
        assert_eq!(info.profile_urls, ["https://x.com/someone"]);
        assert_eq!(info.tags[0].name, "June");
    }
}
