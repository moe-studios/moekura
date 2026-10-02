//! Huashijie works and market products, from its app API. Most need a
//! login: the `userId` and `token` cookies' values, given as
//! `[sources.logins."app.huashijie.art"]`'s `query = { userId = "…", token = "…" }`.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html_to_text, id_of, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = page.rsplit('/').next().unwrap_or_default();
    let api = "https://app.huashijie.art/api";
    let (url, product) = if page.contains("/work/detail/") {
        (
            format!("{api}/work/detail?channel=wap&platform=wap&workId={id}"),
            false,
        )
    } else {
        (
            format!("{api}/product/detail?channel=wap&platform=wap&productId={id}"),
            true,
        )
    };
    let answer = http.json(&url, &[]).await?;
    work_info(known, page, product, &answer["data"]).ok_or_else(|| "Huashijie: no such work".into())
}

fn original(url: &str) -> String {
    moekura_core::sites::parse(url)
        .and_then(|u| u.file_url)
        .unwrap_or_else(|| url.to_owned())
}

fn work_info(known: &SourceUrl, page: &str, product: bool, data: &Value) -> Option<SourceInfo> {
    data.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    if product {
        info.files = strings(&data["imageUrls"], None)
            .iter()
            .map(|u| original(u))
            .collect();
        info.title = text_of(&data["title"]);
        info.description = html_to_text(&text_of(&data["description"]));
    } else {
        info.files = data["multiImages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|image| {
                // A video's first file, else the image.
                let video = image["videoPath"].as_str().filter(|v| !v.is_empty());
                video
                    .and_then(|v| v.split(',').next())
                    .or_else(|| image["orgPath"].as_str())
                    .map(original)
            })
            .collect();
        info.tags = tags_named(strings(&data["faction"], Some("name")));
        info.description = html_to_text(&text_of(&data["content"]));
    }
    let user = &data["user"];
    info.artist_name = user["nick"].as_str().map(|n| n.trim().to_owned());
    if let Some(id) = id_of(&user["id"]) {
        info.artist_account = Some(format!("huashijie_{id}"));
        info.profile_urls
            .push(format!("https://www.huashijie.art/user/index/{id}"));
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn works() {
        let page = "https://www.huashijie.art/work/detail/237016516";
        let known = moekura_core::sites::parse(page).unwrap();
        let data = json!({
            "content": "Cat", "faction": [{ "name": "原创" }], "user": { "id": 13649297, "nick": " Neko " },
            "multiImages": [{ "orgPath": "https://bsyimg.pandapaint.net/v2/work_cover/user/13649297/1714634091547.jpg?x-oss-process=style/work_cover", "videoPath": "" }]
        });
        let info = work_info(&known, page, false, &data).unwrap();
        assert_eq!(
            info.files,
            ["https://bsyimg.pandapaint.net/v2/work_cover/user/13649297/1714634091547.jpg"]
        );
        assert_eq!(
            info.profile_urls,
            ["https://www.huashijie.art/user/index/13649297"]
        );
        assert_eq!(info.tags[0].name, "原创");
    }
}
