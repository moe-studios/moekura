//! VK wall posts, from their mobile pages: the attached photos, the
//! poster, hashtags and text.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let mobile = page.replacen("://vk.com/", "://m.vk.com/", 1);
    let body = http
        .page(&mobile, &[("Cookie", "remixmdevice=1920/1080/1/!")])
        .await?;
    let info = wall_info(known, page, &body);
    if info.files.is_empty() {
        return Err("VK: no photos in the post".into());
    }
    Ok(info)
}

fn wall_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    // A repost's photos are someone else's.
    let repost = html::tags(body, "div")
        .into_iter()
        .any(|t| t.has_class("wall_item") && t.attr("data-copy").is_some());
    if repost {
        return info;
    }
    info.files = html::tags(body, "a")
        .into_iter()
        .filter(|a| {
            a.has_class("PhotoPrimaryAttachment__interactive")
                || a.has_class("MediaGrid__interactive")
        })
        .filter_map(|a| {
            a.attr("data-src_big")
                .or_else(|| a.attr("href"))
                .map(str::to_owned)
        })
        .map(|url| {
            // data-src_big is `<url>|<width>|<height>`.
            let url = url.split('|').next().unwrap_or(&url).to_owned();
            let url = if url.starts_with('/') {
                format!("https://vk.com{url}")
            } else {
                url
            };
            moekura_core::sites::parse(&url)
                .and_then(|u| u.file_url)
                .unwrap_or(url)
        })
        .collect();
    info.artist_name = html::find(body, "a", |t| t.has_class("pi_author"))
        .or_else(|| html::find(body, "span", |t| t.has_class("pi_author")))
        .map(|(_, inner)| html_to_text(inner));
    if let Some((_, text)) = html::find(body, "div", |t| t.has_class("pi_text")) {
        info.tags = tags_named(
            html::tags(text, "a")
                .into_iter()
                .map(|a| html_to_text(html::inner(text, &a)))
                .filter(|t| t.starts_with('#'))
                .map(|t| {
                    t.trim_start_matches('#')
                        .split('@')
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect::<Vec<_>>(),
        );
        info.description = html_to_text(text);
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_posts() {
        let page = "https://vk.com/wall-185765571_2635";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div class="wall_item"><a class="pi_author" href="/club1">Club</a>
            <div class="pi_text">Art <a href="/feed?q=%23cat">#cat@club</a></div>
            <a class="MediaGrid__interactive" data-src_big="https://sun9-69.userapi.com/impg/a/b.jpg?size=1200x1600|1200|1600"></a></div>"#;
        let info = wall_info(&known, page, body);
        assert_eq!(info.files, ["https://pp.userapi.com/a/b.jpg"]);
        assert_eq!(info.tags[0].name, "cat");
        assert_eq!(info.artist_name.as_deref(), Some("Club"));
    }
}
