//! Imgur images and albums, from its API (with the public client id
//! Imgur's own site uses).

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, key, strings, tags_named, text_of};

const CLIENT_ID: &str = "546c25a59c58ad7";

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let (kind, id) = match page.strip_prefix("https://imgur.com/a/") {
        Some(album) => ("posts", album),
        None => ("media", page.trim_start_matches("https://imgur.com/")),
    };
    // `title-<id>`: the id follows the last dash.
    let id = key(id.rsplit('-').next().unwrap_or(id))?;
    let v1 = http
        .json(
            &format!("https://api.imgur.com/post/v1/{kind}/{id}?include=media,tags,account&client_id={CLIENT_ID}"),
            &[],
        )
        .await;
    if let Ok(post) = &v1
        && let Some(info) = v1_info(known, page, post)
    {
        return Ok(info);
    }
    let v3 = match kind {
        "posts" => format!("https://api.imgur.com/3/album/{id}?client_id={CLIENT_ID}"),
        _ => format!("https://api.imgur.com/3/image/{id}?client_id={CLIENT_ID}"),
    };
    let answer = http
        .json(&v3, &[("Authorization", &format!("Client-ID {CLIENT_ID}"))])
        .await?;
    v3_info(known, page, &answer["data"]).ok_or_else(|| "Imgur: no such image".into())
}

fn v1_info(known: &SourceUrl, page: &str, post: &Value) -> Option<SourceInfo> {
    let media = post["media"].as_array().filter(|m| !m.is_empty())?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = media
        .iter()
        .filter_map(|m| m["url"].as_str().map(str::to_owned))
        .collect();
    if let Some(name) = post["account"]["username"]
        .as_str()
        .filter(|n| !n.is_empty())
    {
        info.artist_account = Some(name.to_owned());
        info.profile_urls
            .push(format!("https://imgur.com/user/{name}"));
    }
    info.tags = tags_named(strings(&post["tags"], Some("tag")));
    info.title = text_of(&post["title"]);
    info.description = text_of(&post["description"]);
    Some(info)
}

fn v3_info(known: &SourceUrl, page: &str, data: &Value) -> Option<SourceInfo> {
    let mut info = SourceInfo::new(known.site, page);
    info.files = match data["images"].as_array() {
        Some(images) => images
            .iter()
            .filter_map(|i| i["link"].as_str().map(str::to_owned))
            .collect(),
        None => vec![data["link"].as_str()?.to_owned()],
    };
    if let Some(name) = data["account_url"].as_str() {
        info.artist_account = Some(name.to_owned());
        info.profile_urls
            .push(format!("https://imgur.com/user/{name}"));
    }
    info.title = text_of(&data["title"]);
    info.description = text_of(&data["description"]);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts_and_images() {
        let page = "https://imgur.com/a/0BDNq";
        let known = moekura_core::sites::parse(page).unwrap();
        let post = json!({
            "title": "Cats", "description": "", "tags": [{ "tag": "cat" }],
            "account": { "username": "naugrim" }, "media": [{ "url": "https://i.imgur.com/c7EXjJu.jpeg" }]
        });
        let info = v1_info(&known, page, &post).unwrap();
        assert_eq!(info.files, ["https://i.imgur.com/c7EXjJu.jpeg"]);
        assert_eq!(info.profile_urls, ["https://imgur.com/user/naugrim"]);
        let data = json!({ "link": "https://i.imgur.com/c7EXjJu.png", "account_url": null, "title": null });
        assert_eq!(
            v3_info(&known, page, &data).unwrap().files,
            ["https://i.imgur.com/c7EXjJu.png"]
        );
    }
}
