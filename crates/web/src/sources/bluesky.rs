//! Bluesky posts: `bsky.app/profile/<handle or did>/post/<id>`, read from
//! Bluesky's public API.

use serde_json::Value;
use url::Url;

use super::{Http, SourceInfo, SourceTag, text_of};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub actor: String,
    pub post: String,
}

pub(super) fn target(url: &Url) -> Option<Target> {
    if url.host_str()? != "bsky.app" {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        ["profile", actor, "post", post] => Some(Target {
            actor: (*actor).to_owned(),
            post: (*post).to_owned(),
        }),
        _ => None,
    }
}

pub(super) fn parse(thread: &Value, target: &Target) -> Option<SourceInfo> {
    let post = &thread["thread"]["post"];
    let author = &post["author"];
    let handle = author["handle"].as_str()?.to_owned();
    let embed = &post["embed"];
    // Images, alone or with a quoted post.
    let images = embed["images"]
        .as_array()
        .or_else(|| embed["media"]["images"].as_array());
    let files = images
        .map(|images| {
            images
                .iter()
                .filter_map(|i| i["fullsize"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let tags = post["record"]["facets"]
        .as_array()
        .map(|facets| {
            facets
                .iter()
                .flat_map(|f| f["features"].as_array().into_iter().flatten())
                .filter(|f| f["$type"] == "app.bsky.richtext.facet#tag")
                .filter_map(|f| {
                    Some(SourceTag {
                        name: f["tag"].as_str()?.to_owned(),
                        translation: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut profile_urls = vec![format!("https://bsky.app/profile/{handle}")];
    if let Some(did) = author["did"].as_str() {
        profile_urls.push(format!("https://bsky.app/profile/{did}"));
    }
    Some(SourceInfo {
        site: moekura_core::sites::BLUESKY.name,
        page_url: format!("https://bsky.app/profile/{handle}/post/{}", target.post),
        files,
        headers: Vec::new(),
        artist_name: author["displayName"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_owned),
        artist_account: Some(
            handle
                .strip_suffix(".bsky.social")
                .unwrap_or(&handle)
                .to_owned(),
        ),
        profile_urls,
        tags,
        title: String::new(),
        description: text_of(&post["record"]["text"]),
        ugoira_frames: None,
    })
}

pub(super) async fn fetch(http: &Http<'_>, target: &Target) -> Result<SourceInfo, String> {
    let uri = format!("at://{}/app.bsky.feed.post/{}", target.actor, target.post);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("uri", &uri)
        .append_pair("depth", "0")
        .finish();
    let thread = http
        .json(
            &format!("https://public.api.bsky.app/xrpc/app.bsky.feed.getPostThread?{query}"),
            &[],
        )
        .await?;
    parse(&thread, target).ok_or_else(|| "Bluesky: no such post".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts() {
        let url = Url::parse("https://bsky.app/profile/neko.bsky.social/post/3kabc").unwrap();
        let target = target(&url).unwrap();
        let thread = json!({ "thread": { "post": {
            "author": { "did": "did:plc:xyz", "handle": "neko.bsky.social", "displayName": "Neko" },
            "record": { "text": "A cat #oc", "facets": [
                { "features": [{ "$type": "app.bsky.richtext.facet#tag", "tag": "oc" }] }
            ] },
            "embed": { "images": [{ "fullsize": "https://cdn.bsky.app/img/a@jpeg" }] }
        }}});
        let info = parse(&thread, &target).unwrap();
        assert_eq!(info.files, ["https://cdn.bsky.app/img/a@jpeg"]);
        assert_eq!(info.tags[0].name, "oc");
        assert_eq!(info.profile_urls[1], "https://bsky.app/profile/did:plc:xyz");
        assert_eq!(info.description, "A cat #oc");
    }
}
