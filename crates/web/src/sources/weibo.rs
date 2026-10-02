//! Weibo posts, from the mobile site's page data, read as a visitor
//! (Weibo hands visitors a `SUB` cookie first): the pictures or videos,
//! the poster, topics and text.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, id_of, tags_named, text_of};

/// A visitor's cookies.
async fn visitor(http: &Http<'_>) -> Option<String> {
    let answer = http
        .post_empty(
            "https://passport.weibo.com/visitor/genvisitor?cb=gen_callback",
            &[],
        )
        .await
        .ok()?;
    let data = html::json_after(&answer, "gen_callback(")?;
    let tid = text_of(&data["data"]["tid"]);
    let cookies = http
        .set_cookies(
            &format!("https://passport.weibo.com/visitor/visitor?a=incarnate&t={tid}"),
            &[],
        )
        .await
        .ok()?;
    Some(cookies.join("; "))
}

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    // The mobile page: …/detail/<id> or …/status/<base 62 id>.
    let id = page.rsplit('/').next().unwrap_or_default();
    let mobile = if page.starts_with("https://m.weibo.cn/") {
        page.to_owned()
    } else {
        format!("https://m.weibo.cn/status/{id}")
    };
    let cookie = if http.has_login("weibo.cn") {
        None
    } else {
        visitor(http).await
    };
    let headers: Vec<(&str, &str)> = cookie.iter().map(|c| ("Cookie", c.as_str())).collect();
    let body = http.page(&mobile, &headers).await?;
    let data = html::json_after(&body, "var $render_data =").ok_or("Weibo: no post in the page")?;
    status_info(known, page, &data[0]["status"]).ok_or_else(|| "Weibo: no such post".into())
}

fn status_info(known: &SourceUrl, page: &str, status: &Value) -> Option<SourceInfo> {
    let user = &status["user"];
    let user_id = id_of(&user["id"])?;
    let page_url = match status["bid"].as_str() {
        Some(bid) => format!("https://www.weibo.com/{user_id}/{bid}"),
        None => page.to_owned(),
    };
    let mut info = SourceInfo::new(known.site, page_url);
    info.files = if status["page_info"]["type"] == "video" {
        let media = &status["page_info"]["media_info"];
        [&media["stream_url_hd"], &media["stream_url"]]
            .into_iter()
            .find_map(|u| u.as_str().filter(|u| !u.is_empty()))
            .map(str::to_owned)
            .into_iter()
            .collect()
    } else {
        status["pics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["large"]["url"].as_str().or_else(|| p["url"].as_str()))
            .map(|url| {
                moekura_core::sites::parse(url)
                    .and_then(|u| u.file_url)
                    .unwrap_or_else(|| url.to_owned())
            })
            .collect()
    };
    info.artist_name = user["screen_name"].as_str().map(str::to_owned);
    info.artist_account = Some(format!("weibo_{user_id}"));
    info.profile_urls
        .push(format!("https://www.weibo.com/u/{user_id}"));
    let text = text_of(&status["text"]);
    // Topics are `#topic#` links.
    info.tags = tags_named(
        html::tags(&text, "span")
            .into_iter()
            .filter(|s| s.has_class("surl-text"))
            .map(|s| html_to_text(html::inner(&text, &s)))
            .filter(|t| t.starts_with('#') && t.ends_with('#') && t.len() > 2)
            .map(|t| t.trim_matches('#').to_owned())
            .collect::<Vec<_>>(),
    );
    info.description = html_to_text(&text);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts() {
        let page = "https://www.weibo.com/5501756072/IF9fugHzj";
        let known = moekura_core::sites::parse(page).unwrap();
        let status = json!({
            "bid": "IF9fugHzj", "text": "New <a href=\"x\"><span class=\"surl-text\">#原神#</span></a>",
            "user": { "id": 5501756072_u64, "screen_name": "Neko" },
            "pics": [{ "url": "https://wx1.sinaimg.cn/orj360/a.jpg", "large": { "url": "https://wx1.sinaimg.cn/large/a.jpg" } }]
        });
        let info = status_info(&known, page, &status).unwrap();
        assert_eq!(info.files, ["https://wx1.sinaimg.cn/large/a.jpg"]);
        assert_eq!(info.profile_urls, ["https://www.weibo.com/u/5501756072"]);
        assert_eq!(info.tags[0].name, "原神");
    }
}
