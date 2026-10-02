//! pixivFANBOX posts: `<creator>.fanbox.cc/posts/<id>` or
//! `fanbox.cc/@<creator>/posts/<id>`, read from Fanbox's web API. Posts
//! for paying supporters only have no files for us.

use serde_json::Value;
use url::Url;

use super::{Http, SourceInfo, SourceTag, text_of};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub id: u64,
}

const ORIGIN: &str = "https://www.fanbox.cc";

pub(super) fn target(url: &Url) -> Option<Target> {
    let host = url.host_str()?;
    if !(host == "fanbox.cc" || host.ends_with(".fanbox.cc")) {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let id = match segments.as_slice() {
        ["posts", id] | [_, "posts", id] => id.parse().ok()?,
        _ => return None,
    };
    Some(Target { id })
}

pub(super) fn parse(post: &Value, target: &Target) -> Option<SourceInfo> {
    let body = &post["body"];
    let creator = body["creatorId"].as_str()?.to_owned();
    let content = &body["body"];
    let mut files: Vec<String> = content["images"]
        .as_array()
        .map(|images| {
            images
                .iter()
                .filter_map(|i| i["originalUrl"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    // Articles: images in the order the blocks use them.
    if files.is_empty()
        && let (Some(blocks), Some(map)) = (
            content["blocks"].as_array(),
            content["imageMap"].as_object(),
        )
    {
        files = blocks
            .iter()
            .filter(|b| b["type"] == "image")
            .filter_map(|b| map.get(b["imageId"].as_str()?)?["originalUrl"].as_str())
            .map(str::to_owned)
            .collect();
    }
    let text = content["text"].as_str().map_or_else(
        || {
            content["blocks"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        },
        str::to_owned,
    );
    let mut profile_urls = vec![format!("https://{creator}.fanbox.cc")];
    if let Some(user) = body["user"]["userId"].as_str() {
        profile_urls.push(format!("https://www.pixiv.net/users/{user}"));
    }
    Some(SourceInfo {
        site: moekura_core::sites::FANBOX.name,
        page_url: format!("https://{creator}.fanbox.cc/posts/{}", target.id),
        files,
        headers: vec![
            ("Referer", format!("{ORIGIN}/")),
            ("Origin", ORIGIN.to_owned()),
        ],
        artist_name: body["user"]["name"].as_str().map(str::to_owned),
        artist_account: Some(creator.clone()),
        profile_urls,
        tags: body["tags"]
            .as_array()
            .map(|tags| {
                tags.iter()
                    .filter_map(|t| {
                        Some(SourceTag {
                            name: t.as_str()?.to_owned(),
                            translation: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        title: text_of(&body["title"]),
        description: text.trim().to_owned(),
        ugoira_frames: None,
    })
}

pub(super) async fn fetch(http: &Http<'_>, target: &Target) -> Result<SourceInfo, String> {
    let post = http
        .json(
            &format!("https://api.fanbox.cc/post.info?postId={}", target.id),
            &[("Origin", ORIGIN), ("Referer", "https://www.fanbox.cc/")],
        )
        .await?;
    parse(&post, target).ok_or_else(|| "Fanbox: no such post".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts() {
        let url = Url::parse("https://www.fanbox.cc/@neko/posts/55").unwrap();
        let target = target(&url).unwrap();
        let post = json!({ "body": {
            "title": "New", "creatorId": "neko", "tags": ["cat"],
            "user": { "userId": "5", "name": "Neko" },
            "body": { "blocks": [
                { "type": "p", "text": "Hello" },
                { "type": "image", "imageId": "a" }
            ], "imageMap": { "a": { "originalUrl": "https://downloads.fanbox.cc/a.png" } } }
        }});
        let info = parse(&post, &target).unwrap();
        assert_eq!(info.files, ["https://downloads.fanbox.cc/a.png"]);
        assert_eq!(info.page_url, "https://neko.fanbox.cc/posts/55");
        assert_eq!(info.description, "Hello");
        assert_eq!(info.profile_urls[1], "https://www.pixiv.net/users/5");
    }
}
