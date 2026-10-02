//! Newgrounds art and movies, from their pages (and a movie's video
//! sources). Mature works need a login (the `ng_remember` cookie).

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    if let Some(id) = page.strip_prefix("https://www.newgrounds.com/portal/view/") {
        let video = http
            .json(
                &format!("https://www.newgrounds.com/portal/video/{id}"),
                &[("X-Requested-With", "XMLHttpRequest")],
            )
            .await?;
        let body = http.page(page, &[]).await.unwrap_or_default();
        let mut info = art_info(known, page, &body);
        info.files = best_video(&video).into_iter().collect();
        return Ok(info);
    }
    let body = http.page(page, &[]).await?;
    let info = art_info(known, page, &body);
    if info.files.is_empty() {
        return Err("Newgrounds: no art in the page".into());
    }
    Ok(info)
}

/// The largest of a movie's sources (`{"sources": {"1080p": [{"src": …}]}}`).
fn best_video(video: &Value) -> Option<String> {
    let sources = video["sources"].as_object()?;
    let (_, best) = sources
        .iter()
        .max_by_key(|(size, _)| size.trim_end_matches('p').parse::<u32>().unwrap_or(0))?;
    best[0]["src"].as_str().map(str::to_owned)
}

fn art_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    // A gallery of several lists them in a script.
    if let Some(images) = html::json_after(body, "let imageData =") {
        info.files = super::strings(&images, Some("image"));
    }
    if info.files.is_empty() {
        let images = html::find(body, "div", |t| t.has_class("art-images"))
            .or_else(|| html::find(body, "div", |t| t.has_class("image")))
            .map(|(_, inner)| inner)
            .unwrap_or_default();
        info.files = html::tags(images, "a")
            .into_iter()
            .filter(|a| a.attr("data-action").is_some())
            .filter_map(|a| a.attr("href").map(str::to_owned))
            .chain(
                html::tags(images, "img")
                    .into_iter()
                    .filter_map(|i| i.attr("src").map(str::to_owned)),
            )
            .collect();
    }
    info.files.dedup();
    if let Some((_, user)) = html::find(body, "div", |t| t.has_class("item-user"))
        && let Some((link, name)) = html::find(user, "h4", |_| true)
            .and_then(|(_, h4)| html::find(h4, "a", |_| true).map(|(t, n)| (t, html_to_text(n))))
    {
        info.artist_name = Some(name);
        if let Some(profile) = link
            .attr("href")
            .and_then(|h| moekura_core::sites::parse(h)?.profile_url)
        {
            info.artist_account = profile
                .strip_prefix("https://")
                .and_then(|h| h.strip_suffix(".newgrounds.com"))
                .map(str::to_owned);
            info.profile_urls.push(profile);
        }
    }
    info.tags = tags_named(
        html::find(body, "dd", |t| t.has_class("tags"))
            .or_else(|| html::find(body, "div", |t| t.has_class("tags")))
            .map(|(_, tags)| {
                html::tags(tags, "a")
                    .into_iter()
                    .map(|a| html_to_text(html::inner(tags, &a)).replace('-', "_"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    info.title = html::find(body, "h2", |t| t.attr("itemprop") == Some("name"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::find(body, "div", |t| t.attr("id") == Some("author_comments"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn art_and_movies() {
        let page = "https://www.newgrounds.com/art/view/natthelich/pandora";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div class="item-user"><div class="item-details"><h4><a href="https://natthelich.newgrounds.com">NatTheLich</a></h4></div></div>
            <h2 itemprop="name">Pandora</h2>
            <div class="image"><img src="https://art.ngfiles.com/images/1254000/1254722_natthelich_pandora.jpg"></div>
            <div id="author_comments"><p>Done</p></div><dd class="tags"><a href="/t">fire-emblem</a></dd>"#;
        let info = art_info(&known, page, body);
        assert_eq!(
            info.files,
            ["https://art.ngfiles.com/images/1254000/1254722_natthelich_pandora.jpg"]
        );
        assert_eq!(info.artist_account.as_deref(), Some("natthelich"));
        assert_eq!(info.tags[0].name, "fire_emblem");
        let video = json!({ "sources": { "360p": [{ "src": "https://x/360.mp4" }], "1080p": [{ "src": "https://x/1080.mp4" }] } });
        assert_eq!(best_video(&video).as_deref(), Some("https://x/1080.mp4"));
    }
}
