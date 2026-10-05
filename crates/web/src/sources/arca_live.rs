//! Arca.live posts, from the app's API: the post's images and videos, at
//! their originals.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, id_of, number, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = number(page.rsplit('/').next().unwrap_or_default())?;
    let api = http
        .json(
            &format!("https://arca.live/api/app/view/article/breaking/{id}"),
            &[("User-Agent", "net.umanle.arca.android.playstore/0.9.75")],
        )
        .await?;
    post_info(known, page, &api).ok_or_else(|| "Arca.live: no such post".into())
}

fn post_info(known: &SourceUrl, page: &str, api: &Value) -> Option<SourceInfo> {
    let content = api["content"].as_str()?;
    let mut info = SourceInfo::new(known.site, page);
    if let (Some(board), Some(id)) = (api["boardSlug"].as_str(), id_of(&api["id"])) {
        info.page_url = format!("https://arca.live/b/{board}/{id}");
    }
    let absolute = |src: &str| {
        if src.starts_with("//") {
            format!("https:{src}")
        } else {
            src.to_owned()
        }
    };
    info.files = html::tags(content, "img")
        .into_iter()
        .chain(html::tags(content, "video"))
        .filter(|t| !t.has_class("arca-emoticon"))
        .filter_map(|t| {
            t.attr("data-originalurl")
                .or_else(|| t.attr("src"))
                .map(absolute)
        })
        .map(|url| {
            moekura_core::sites::parse(&url)
                .and_then(|u| u.file_url)
                .unwrap_or(url)
        })
        .collect();
    if let Some(name) = api["nickname"].as_str() {
        info.artist_name = Some(name.to_owned());
        info.profile_urls.push(match id_of(&api["publicId"]) {
            Some(id) => format!("https://arca.live/u/@{name}/{id}"),
            None => format!("https://arca.live/u/@{name}"),
        });
    }
    info.title = text_of(&api["title"]);
    info.description = html_to_text(content);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts() {
        let page = "https://arca.live/b/arknights/66031722";
        let known = moekura_core::sites::parse(page).unwrap();
        let api = json!({
            "id": 66031722, "boardSlug": "arknights", "title": "Art", "nickname": "Nauju", "publicId": 45320365,
            "content": "<p>Hi</p><img src=\"//ac2.namu.la/20221225sac2/e06dcf8edd29c597240898a6752c74dbdd0680fc932cfd0ecc898795f1db34b5.jpg\"><img class=\"arca-emoticon\" src=\"//x/e.png\">"
        });
        let info = post_info(&known, page, &api).unwrap();
        assert_eq!(
            info.files,
            [
                "https://ac2.namu.la/20221225sac2/e06dcf8edd29c597240898a6752c74dbdd0680fc932cfd0ecc898795f1db34b5.jpg?type=orig"
            ]
        );
        assert_eq!(info.profile_urls, ["https://arca.live/u/@Nauju/45320365"]);
        assert_eq!(info.description, "Hi");
    }
}
