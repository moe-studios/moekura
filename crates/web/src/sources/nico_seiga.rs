//! Nico Seiga illustrations and manga. The illustration's details come
//! from the mobile site's JSON; its file is where `/image/source/<id>`
//! redirects, which needs a login (the `user_session` cookie).

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html_to_text, id_of, strings, tags_named, text_of};

const HEADERS: [(&str, &str); 1] = [("Cookie", "skip_fetish_warning=1")];

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    if let Some(id) = page.strip_prefix("https://manga.nicovideo.jp/watch/mg") {
        let theme = http
            .json(
                &format!("https://seiga.nicovideo.jp/api/theme/info?id={id}"),
                &HEADERS,
            )
            .await
            .unwrap_or_default();
        let frames = http
            .json(
                &format!("https://api.nicomanga.jp/api/v1/app/manga/episodes/{id}/frames?enable_webp=false"),
                &[],
            )
            .await?;
        let mut info = SourceInfo::new(known.site, page);
        info.files = frames["data"]["result"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["meta"]["source_url"].as_str().map(str::to_owned))
            .collect();
        info.title = text_of(&theme["response"]["theme"]["title"]);
        return Ok(info);
    }
    let id = page
        .strip_prefix("https://seiga.nicovideo.jp/seiga/im")
        .ok_or("Nico Seiga: not an illustration")?;
    let answer = http
        .json(
            &format!("https://sp.seiga.nicovideo.jp/ajax/seiga/im{id}"),
            &HEADERS,
        )
        .await?;
    let mut info = illust_info(known, page, &answer["target_image"])
        .ok_or("Nico Seiga: no such illustration")?;
    // The original is under /priv/, which the source link redirects to.
    let source = http
        .final_url(
            &format!("https://seiga.nicovideo.jp/image/source/{id}"),
            &HEADERS,
        )
        .await?;
    let path = source.path();
    if let Some(tail) = path
        .strip_prefix("/o/")
        .or_else(|| path.strip_prefix("/priv/"))
    {
        info.files = vec![format!("https://lohas.nicoseiga.jp/priv/{tail}")];
    }
    Ok(info)
}

fn illust_info(known: &SourceUrl, page: &str, image: &Value) -> Option<SourceInfo> {
    image.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    info.artist_name = image["nickname"].as_str().map(str::to_owned);
    if let Some(user) = id_of(&image["user_id"]).filter(|u| u != "0") {
        info.artist_account = Some(format!("nicoseiga_{user}"));
        info.profile_urls
            .push(format!("https://seiga.nicovideo.jp/user/illust/{user}"));
    }
    info.tags = tags_named(strings(&image["tag_list"]["tag"], Some("name")));
    info.title = text_of(&image["title"]);
    info.description = html_to_text(&text_of(&image["description"]));
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn illustrations() {
        let page = "https://seiga.nicovideo.jp/seiga/im3521156";
        let known = moekura_core::sites::parse(page).unwrap();
        let image = json!({
            "title": "Cat", "description": "Hi<br>there", "nickname": "Neko", "user_id": "456831",
            "tag_list": { "tag": [{ "name": "猫" }] }
        });
        let info = illust_info(&known, page, &image).unwrap();
        assert_eq!(
            info.profile_urls,
            ["https://seiga.nicovideo.jp/user/illust/456831"]
        );
        assert_eq!(info.tags[0].name, "猫");
        assert_eq!(info.description, "Hi\nthere");
    }
}
