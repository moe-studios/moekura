//! YouTube community posts, from the page's initial data: the post's
//! images, the channel, hashtags and text. Videos are read for their
//! OpenGraph tags.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, key, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let Some(post) = page.strip_prefix("https://www.youtube.com/post/") else {
        return Err("YouTube: only community posts have images".into());
    };
    // A channel's link names its post in the query, where anything could be.
    key(post)?;
    let body = http.page(page, &[]).await?;
    let data =
        html::json_after(&body, "var ytInitialData =").ok_or("YouTube: no post in the page")?;
    let post = &data["contents"]["twoColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]
        ["sectionListRenderer"]["contents"][0]["itemSectionRenderer"]["contents"][0]["backstagePostThreadRenderer"]
        ["post"]["backstagePostRenderer"];
    post_info(known, page, post).ok_or_else(|| "YouTube: no such post".into())
}

fn post_info(known: &SourceUrl, page: &str, post: &Value) -> Option<SourceInfo> {
    post.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    let attachment = &post["backstageAttachment"];
    let images: Vec<&Value> = match attachment["postMultiImageRenderer"]["images"].as_array() {
        Some(images) => images.iter().collect(),
        None => vec![attachment],
    };
    info.files = images
        .iter()
        .filter_map(|a| {
            a["backstageImageRenderer"]["image"]["thumbnails"]
                .as_array()?
                .last()?["url"]
                .as_str()
        })
        .map(|url| {
            moekura_core::sites::parse(url)
                .and_then(|u| u.file_url)
                .unwrap_or_else(|| url.to_owned())
        })
        .collect();
    info.artist_name = post["authorText"]["runs"][0]["text"]
        .as_str()
        .map(str::to_owned);
    let endpoint = &post["authorEndpoint"]["browseEndpoint"];
    if let Some(handle) = endpoint["canonicalBaseUrl"]
        .as_str()
        .and_then(|h| h.strip_prefix("/@"))
    {
        info.artist_account = Some(handle.to_owned());
        info.profile_urls
            .push(format!("https://www.youtube.com/@{handle}"));
    }
    if let Some(channel) = endpoint["browseId"].as_str() {
        info.profile_urls
            .push(format!("https://www.youtube.com/channel/{channel}"));
    }
    let runs = post["contentText"]["runs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    info.description = runs.iter().filter_map(|r| r["text"].as_str()).collect();
    info.tags = tags_named(
        runs.iter()
            .filter_map(|r| r["text"].as_str())
            .filter(|t| t.starts_with('#'))
            .map(|t| t.trim_start_matches('#')),
    );
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn community_posts() {
        let page = "https://www.youtube.com/post/UgkxWevNfezmf";
        let known = moekura_core::sites::parse(page).unwrap();
        let post = json!({
            "authorText": { "runs": [{ "text": "Rui" }] },
            "authorEndpoint": { "browseEndpoint": { "browseId": "UCyk", "canonicalBaseUrl": "/@nonomaRui" } },
            "contentText": { "runs": [{ "text": "New art " }, { "text": "#art" }] },
            "backstageAttachment": { "backstageImageRenderer": { "image": { "thumbnails": [
                { "url": "https://yt3.ggpht.com/abc=s288" }, { "url": "https://yt3.ggpht.com/abc=s1024" }
            ] } } }
        });
        let info = post_info(&known, page, &post).unwrap();
        assert_eq!(info.files, ["https://yt3.ggpht.com/abc=d"]);
        assert_eq!(
            info.profile_urls,
            [
                "https://www.youtube.com/@nonomaRui",
                "https://www.youtube.com/channel/UCyk"
            ]
        );
        assert_eq!(info.tags[0].name, "art");
    }
}
