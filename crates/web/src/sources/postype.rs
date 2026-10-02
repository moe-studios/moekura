//! Postype posts, from its APIs: the post's images, the blog and
//! profile, tags and text. Paid and adult posts need a login (the `PSE3`
//! cookie).

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = page.rsplit('/').next().unwrap_or_default();
    let headers = [("Referer", "https://postype.com")];
    let post = http
        .json(
            &format!("https://api.postype.com/api/v1/posts/{id}"),
            &headers,
        )
        .await?;
    let content = http
        .json(
            &format!("https://www.postype.com/api/post/content/{id}"),
            &headers,
        )
        .await
        .unwrap_or_default();
    post_info(known, page, &post, &content).ok_or_else(|| "Postype: no such post".into())
}

fn post_info(known: &SourceUrl, page: &str, post: &Value, content: &Value) -> Option<SourceInfo> {
    post.as_object()?;
    let body = text_of(&content["data"]["html"]);
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(&body, "img")
        .into_iter()
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .map(|src| {
            moekura_core::sites::parse(&src)
                .and_then(|u| u.file_url)
                .unwrap_or(src)
        })
        .collect();
    info.artist_name = post["profile"]["nickname"].as_str().map(str::to_owned);
    if let Some(blog) = post["channel"]["name"].as_str() {
        info.profile_urls
            .push(format!("https://www.postype.com/@{blog}"));
    }
    if let Some(hash) = post["profile"]["hash"].as_str() {
        info.profile_urls
            .push(format!("https://www.postype.com/profile/@{hash}"));
    }
    info.tags = tags_named(strings(&post["tags"], None));
    info.title = text_of(&post["title"]);
    let subtitle = text_of(&post["subTitle"]);
    info.description = [subtitle, html_to_text(&body)]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts() {
        let page = "https://www.postype.com/@luland/post/11659399";
        let known = moekura_core::sites::parse(page).unwrap();
        let post = json!({ "title": "Art", "subTitle": "", "tags": ["cat"],
            "channel": { "name": "luland" }, "profile": { "nickname": "Lu", "hash": "ep58bc" } });
        let content = json!({ "data": { "html": "<p>Hi</p><figure><img src=\"https://d2ufj6gm1gtdrc.cloudfront.net/2018/09/10/22/49/e91a.jpg?w=1200&amp;q=90\"></figure>" } });
        let info = post_info(&known, page, &post, &content).unwrap();
        assert_eq!(
            info.files,
            ["https://d2ufj6gm1gtdrc.cloudfront.net/2018/09/10/22/49/e91a.jpg"]
        );
        assert_eq!(
            info.profile_urls,
            [
                "https://www.postype.com/@luland",
                "https://www.postype.com/profile/@ep58bc"
            ]
        );
        assert_eq!(info.description, "Hi");
    }
}
