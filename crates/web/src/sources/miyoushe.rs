//! Miyoushe and HoYoLAB articles, from their APIs: the images (or the
//! best video), the author, topics and text.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, id_of, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = page.rsplit('/').next().unwrap_or_default();
    let hoyolab = page.starts_with("https://www.hoyolab.com/");
    let api = if hoyolab {
        format!("https://bbs-api-os.hoyolab.com/community/post/wapi/getPostFull?post_id={id}")
    } else {
        format!("https://bbs-api.miyoushe.com/post/wapi/getPostFull?post_id={id}")
    };
    let answer = http
        .json(&api, &[("Referer", "https://www.miyoushe.com")])
        .await?;
    let base = if hoyolab {
        "https://www.hoyolab.com"
    } else {
        "https://www.miyoushe.com/sr"
    };
    article_info(known, page, base, &answer["data"]["post"])
        .ok_or_else(|| "Miyoushe: no such article".into())
}

fn article_info(known: &SourceUrl, page: &str, base: &str, article: &Value) -> Option<SourceInfo> {
    article.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    let videos: Vec<String> = article["vod_list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            v["resolutions"]
                .as_array()?
                .iter()
                .max_by_key(|r| r["bitrate"].as_u64().unwrap_or(0))?["url"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    info.files = if videos.is_empty() {
        strings(&article["image_list"], Some("url"))
    } else {
        videos
    };
    let user = &article["user"];
    info.artist_name = user["nickname"].as_str().map(str::to_owned);
    if let Some(id) = id_of(&user["uid"]) {
        info.profile_urls
            .push(format!("{base}/accountCenter/postList?id={id}"));
    }
    info.tags = tags_named(strings(&article["topics"], Some("name")));
    let post = &article["post"];
    info.title = text_of(&post["subject"])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // The text is a list of inserts (Quill's), or JSON with a description.
    let structured: Value = post["structured_content"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    info.description = match structured.as_array() {
        Some(ops) => ops
            .iter()
            .filter_map(|op| op["insert"].as_str())
            .collect::<String>()
            .trim()
            .to_owned(),
        None => post["content"]
            .as_str()
            .and_then(|c| serde_json::from_str::<Value>(c).ok())
            .map(|c| text_of(&c["describe"]))
            .unwrap_or_else(|| text_of(&post["content"])),
    };
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn articles() {
        let page = "https://www.miyoushe.com/bh3/article/28939887";
        let known = moekura_core::sites::parse(page).unwrap();
        let article = json!({
            "image_list": [{ "url": "https://upload-bbs.miyoushe.com/upload/2022/09/14/73731802/a.jpg" }],
            "user": { "uid": "73731802", "nickname": "Neko" }, "topics": [{ "name": "Kiana" }],
            "post": { "subject": "New  art", "structured_content": "[{\"insert\":\"Hello\\n\"},{\"insert\":{\"image\":\"x\"}}]" }
        });
        let info = article_info(&known, page, "https://www.miyoushe.com/sr", &article).unwrap();
        assert_eq!(
            info.profile_urls,
            ["https://www.miyoushe.com/sr/accountCenter/postList?id=73731802"]
        );
        assert_eq!(info.title, "New art");
        assert_eq!(info.description, "Hello");
    }
}
