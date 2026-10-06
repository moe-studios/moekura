//! Reddit posts, from `/comments/<id>.json`: an image post's image or a
//! gallery's, the poster, the flair and the text. Share links (`/s/…`)
//! are followed to the post.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, decode_entities, html_to_text, key, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let page = if page.contains("/s/") {
        http.final_url(page, &[]).await?.to_string()
    } else {
        page.to_owned()
    };
    let parts: Vec<&str> = page.split('/').collect();
    let id = parts
        .iter()
        .position(|p| *p == "comments")
        .and_then(|at| parts.get(at + 1))
        .ok_or("Reddit: not a post")?;
    let id = key(id)?;
    let answer = http
        .json(&format!("https://www.reddit.com/comments/{id}.json"), &[])
        .await?;
    post_info(known, &answer[0]["data"]["children"][0]["data"])
        .ok_or_else(|| "Reddit: no such post".into())
}

fn post_info(known: &SourceUrl, post: &Value) -> Option<SourceInfo> {
    let permalink = post["permalink"].as_str()?;
    let permalink = permalink.replacen("/r/u_", "/user/", 1);
    let mut info = SourceInfo::new(known.site, format!("https://www.reddit.com{permalink}"));
    let original = |url: &str| {
        let url = decode_entities(url);
        moekura_core::sites::parse(&url)
            .and_then(|u| u.file_url)
            .unwrap_or(url)
    };
    if post["crosspost_parent"].is_null() {
        if post["is_reddit_media_domain"] == true {
            info.files = post["preview"]["images"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|i| i["source"]["url"].as_str())
                .map(original)
                .collect();
        }
        if let Some(media) = post["media_metadata"].as_object() {
            let order: Vec<&str> = post["gallery_data"]["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|i| i["media_id"].as_str())
                .collect();
            let ids: Vec<&String> = if order.is_empty() {
                media.keys().collect()
            } else {
                media
                    .keys()
                    .filter(|k| order.contains(&k.as_str()))
                    .collect()
            };
            let mut gallery: Vec<(usize, String)> = ids
                .into_iter()
                .filter_map(|k| {
                    let at = order.iter().position(|o| o == k).unwrap_or(usize::MAX);
                    Some((at, original(media[k]["s"]["u"].as_str()?)))
                })
                .collect();
            gallery.sort();
            info.files.extend(gallery.into_iter().map(|(_, url)| url));
        }
    }
    if let Some(author) = post["author"].as_str().filter(|a| *a != "[deleted]") {
        info.artist_name = Some(author.to_owned());
        info.artist_account = Some(author.to_owned());
        info.profile_urls
            .push(format!("https://www.reddit.com/user/{author}"));
    }
    info.tags = tags_named(post["link_flair_text"].as_str());
    info.title = text_of(&post["title"]);
    info.description = html_to_text(&decode_entities(&text_of(&post["selftext_html"])));
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts_and_galleries() {
        let known = moekura_core::sites::parse("https://www.reddit.com/comments/ttyccp").unwrap();
        let post = json!({
            "permalink": "/r/arknights/comments/ttyccp/maria/", "author": "Artist", "title": "Maria",
            "link_flair_text": "Fanart", "is_reddit_media_domain": false, "crosspost_parent": null,
            "selftext_html": "&lt;p&gt;Hi&lt;/p&gt;",
            "gallery_data": { "items": [{ "media_id": "b" }, { "media_id": "a" }] },
            "media_metadata": {
                "a": { "s": { "u": "https://preview.redd.it/aaa.jpg?width=1&amp;s=x" } },
                "b": { "s": { "u": "https://preview.redd.it/bbb.png?width=1&amp;s=x" } }
            }
        });
        let info = post_info(&known, &post).unwrap();
        assert_eq!(
            info.page_url,
            "https://www.reddit.com/r/arknights/comments/ttyccp/maria/"
        );
        assert_eq!(
            info.files,
            ["https://i.redd.it/bbb.png", "https://i.redd.it/aaa.jpg"]
        );
        assert_eq!(info.tags[0].name, "Fanart");
        assert_eq!(info.description, "Hi");
    }
}
