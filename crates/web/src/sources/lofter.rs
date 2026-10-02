//! Lofter posts, from the data their (mobile) pages start with: photos,
//! videos, text posts' images and answers, the blog, tags and text.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, id_of, strings, tags_named, text_of};

const AGENT: (&str, &str) = (
    "User-Agent",
    "Mozilla/5.0 (Android 14; Mobile; rv:115.0) Gecko/115.0 Firefox/115.0",
);

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[AGENT]).await?;
    let data = html::json_after(&body, "window.__initialize_data__ =")
        .ok_or("Lofter: no post in the page")?;
    post_info(known, page, &data["postData"]["data"]).ok_or_else(|| "Lofter: no such post".into())
}

fn post_info(known: &SourceUrl, page: &str, data: &Value) -> Option<SourceInfo> {
    let post = &data["postData"]["postView"];
    post.as_object()?;
    let blog = &data["blogInfo"];
    let mut info = SourceInfo::new(known.site, page);
    let mut files = strings(&post["photoPostView"]["photoLinks"], Some("orign"));
    files.extend(
        post["videoPostView"]["videoInfo"]["originUrl"]
            .as_str()
            .map(str::to_owned),
    );
    let text = text_of(&post["textPostView"]["content"]);
    files.extend(
        html::tags(&text, "img")
            .into_iter()
            .filter_map(|i| i.attr("src").map(str::to_owned)),
    );
    files.extend(strings(&post["answerPostView"]["images"], Some("orign")));
    info.files = files
        .into_iter()
        .map(|url| {
            moekura_core::sites::parse(&url)
                .and_then(|u| u.file_url)
                .unwrap_or(url)
        })
        .collect();
    info.artist_name = blog["blogNickName"].as_str().map(|n| n.trim().to_owned());
    if let Some(name) = blog["blogName"].as_str() {
        info.artist_account = Some(name.to_owned());
        info.profile_urls.push(format!("https://{name}.lofter.com"));
    }
    if let Some(id) = id_of(&blog["blogId"]) {
        info.profile_urls.push(format!(
            "https://www.lofter.com/mentionredirect.do?blogId={id}"
        ));
    }
    info.tags = tags_named(strings(&post["tagList"], None));
    info.title = match post["answerPostView"]["questionInfo"]["question"].as_str() {
        Some(question) => format!("Q:{question}"),
        None => text_of(&post["title"]),
    };
    let description = [
        &post["photoPostView"]["caption"],
        &post["videoPostView"]["caption"],
        &post["textPostView"]["content"],
        &post["answerPostView"]["answer"],
    ]
    .into_iter()
    .find_map(|v| v.as_str())
    .unwrap_or_default();
    info.description = html_to_text(description);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts() {
        let page = "https://gengar563.lofter.com/post/1e82da8c_1c98dae1b";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<body><script>window.__initialize_data__ = {"postData": {"data": {
            "blogInfo": {"blogName": "gengar563", "blogNickName": " Gengar ", "blogId": 1278105311},
            "postData": {"postView": {"title": "", "tagList": ["pokemon"],
                "photoPostView": {"caption": "<p>Hi</p>", "photoLinks": [{"orign": "https://imglf3.lf127.net/img/abc.png?imageView&thumbnail=1680x0"}]}}}}}}</script></body>"#;
        let data = html::json_after(body, "window.__initialize_data__ =").unwrap();
        let info = post_info(&known, page, &data["postData"]["data"]).unwrap();
        assert_eq!(info.files, ["https://imglf3.lf127.net/img/abc.png"]);
        assert_eq!(
            info.profile_urls,
            [
                "https://gengar563.lofter.com",
                "https://www.lofter.com/mentionredirect.do?blogId=1278105311"
            ]
        );
        assert_eq!(info.description, "Hi");
    }
}
