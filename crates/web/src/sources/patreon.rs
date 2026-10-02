//! Patreon posts, from its API (JSON:API): the post's media, the creator,
//! tags and text. Posts for patrons need a login (the `session_id`
//! cookie) to show their media.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html_to_text, id_of, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let slug = page
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let id = slug.rsplit('-').next().unwrap_or(slug);
    let answer = http
        .json(
            &format!(
                "https://www.patreon.com/api/posts/{id}?include=media,images,attachments,user,user_defined_tags,poll.choices"
            ),
            &[],
        )
        .await?;
    post_info(known, page, &answer).ok_or_else(|| "Patreon: no such post".into())
}

/// Patreon's rich text (`content_json_string`), as plain text.
fn rich_text(node: &Value, out: &mut String) {
    match node {
        Value::Array(children) => children.iter().for_each(|c| rich_text(c, out)),
        Value::Object(map) => {
            match map.get("type").and_then(Value::as_str) {
                Some("text") => {
                    out.push_str(map.get("text").and_then(Value::as_str).unwrap_or_default())
                }
                Some("hardBreak") => out.push('\n'),
                _ => {}
            }
            if let Some(children) = map.get("content") {
                rich_text(children, out);
            }
            if matches!(
                map.get("type").and_then(Value::as_str),
                Some("paragraph" | "heading" | "listItem" | "blockquote")
            ) {
                out.push('\n');
            }
        }
        _ => {}
    }
}

fn post_info(known: &SourceUrl, page: &str, answer: &Value) -> Option<SourceInfo> {
    let post = &answer["data"]["attributes"];
    post.as_object()?;
    let included = answer["included"].as_array().cloned().unwrap_or_default();
    let of_type = |kind: &'static str| included.iter().filter(move |i| i["type"] == kind);
    let mut info = SourceInfo::new(known.site, page);
    let mut seen = Vec::new();
    for media in of_type("media") {
        let attrs = &media["attributes"];
        // Old posts list an inline image twice.
        let key = (text_of(&attrs["file_name"]), attrs["size_bytes"].as_u64());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        info.files
            .extend(attrs["display"]["url"].as_str().map(str::to_owned));
    }
    info.headers = vec![("Referer", "https://www.patreon.com".to_owned())];
    if let Some(user) = of_type("user").next() {
        let attrs = &user["attributes"];
        info.artist_name = attrs["full_name"].as_str().map(|n| n.trim().to_owned());
        if let Some(vanity) = attrs["vanity"].as_str() {
            info.artist_account = Some(vanity.to_owned());
            info.profile_urls
                .push(format!("https://www.patreon.com/{vanity}"));
        }
        if let Some(id) = id_of(&user["id"]) {
            info.profile_urls
                .push(format!("https://www.patreon.com/user?u={id}"));
        }
    }
    info.tags = tags_named(
        of_type("post_tag")
            .filter_map(|t| t["attributes"]["value"].as_str())
            .collect::<Vec<_>>(),
    );
    info.title = text_of(&post["title"]);
    let content: Value = post["content_json_string"]
        .as_str()
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or_default();
    let mut text = String::new();
    rich_text(&content, &mut text);
    info.description = if text.trim().is_empty() {
        html_to_text(&text_of(&post["content"]))
    } else {
        text.trim().to_owned()
    };
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts() {
        let page = "https://www.patreon.com/posts/sparkle-71057815";
        let known = moekura_core::sites::parse(page).unwrap();
        let answer = json!({
            "data": { "attributes": {
                "title": "Sparkle",
                "content_json_string": "{\"type\":\"doc\",\"content\":[{\"type\":\"paragraph\",\"content\":[{\"type\":\"text\",\"text\":\"Hi\"}]}]}"
            } },
            "included": [
                { "type": "media", "attributes": { "file_name": "1.jpg", "size_bytes": 5, "display": { "url": "https://c10.patreonusercontent.com/a/1.jpg" } } },
                { "type": "media", "attributes": { "file_name": "1.jpg", "size_bytes": 5, "display": { "url": "https://c10.patreonusercontent.com/b/1.jpg" } } },
                { "type": "user", "id": "4045578", "attributes": { "full_name": "Neko ", "vanity": "neko" } },
                { "type": "post_tag", "attributes": { "value": "cat" } }
            ]
        });
        let info = post_info(&known, page, &answer).unwrap();
        assert_eq!(info.files, ["https://c10.patreonusercontent.com/a/1.jpg"]);
        assert_eq!(
            info.profile_urls,
            [
                "https://www.patreon.com/neko",
                "https://www.patreon.com/user?u=4045578"
            ]
        );
        assert_eq!(info.description, "Hi");
    }
}
