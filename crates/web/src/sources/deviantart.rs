//! DeviantArt works (`deviantart.com/<user>/art/<slug>`, or the older
//! `<user>.deviantart.com/art/<slug>`), read through its oEmbed API.

use serde_json::Value;
use url::Url;

use super::{Http, SourceInfo, SourceTag, text_of};

pub(super) fn matches(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|h| h == "deviantart.com" || h.ends_with(".deviantart.com"))
        && url.path().contains("/art/")
}

pub(super) fn parse(embed: &Value, page_url: &str) -> Option<SourceInfo> {
    let author = embed["author_name"].as_str()?.to_owned();
    let file = embed["url"].as_str().map(str::to_owned);
    let tags = embed["tags"]
        .as_str()
        .map(|tags| {
            tags.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(|t| SourceTag {
                    name: t.to_owned(),
                    translation: None,
                })
                .collect()
        })
        .unwrap_or_default();
    Some(SourceInfo {
        site: "DeviantArt",
        page_url: page_url.to_owned(),
        files: file.into_iter().collect(),
        headers: Vec::new(),
        artist_name: Some(author.clone()),
        artist_account: Some(author.clone()),
        profile_urls: vec![embed["author_url"].as_str().map_or_else(
            || format!("https://www.deviantart.com/{author}"),
            str::to_owned,
        )],
        tags,
        title: text_of(&embed["title"]),
        description: String::new(),
        ugoira_frames: None,
    })
}

pub(super) async fn fetch(http: &Http<'_>, url: &Url) -> Result<SourceInfo, String> {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", url.as_str())
        .append_pair("format", "json")
        .finish();
    let embed = http
        .json(
            &format!("https://backend.deviantart.com/oembed?{query}"),
            &[],
        )
        .await?;
    parse(&embed, url.as_str()).ok_or_else(|| "DeviantArt: no such work".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn works() {
        let embed = json!({
            "title": "Cat", "author_name": "neko", "author_url": "https://www.deviantart.com/neko",
            "url": "https://images-wixmp.example/cat.png", "tags": "cat, cute"
        });
        let info = parse(&embed, "https://www.deviantart.com/neko/art/Cat-1").unwrap();
        assert_eq!(info.files, ["https://images-wixmp.example/cat.png"]);
        assert_eq!(info.tags.len(), 2);
        assert_eq!(info.profile_urls, ["https://www.deviantart.com/neko"]);
    }
}
