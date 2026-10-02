//! Pinterest pins, from its resource API.

use moekura_core::sites::SourceUrl;
use serde_json::{Value, json};

use super::{Http, SourceInfo, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = page
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let data = json!({ "options": { "id": id, "field_set_key": "detailed" } }).to_string();
    let query: String = url::form_urlencoded::byte_serialize(data.as_bytes()).collect();
    let answer = http
        .json(
            &format!("https://www.pinterest.com/resource/PinResource/get/?data={query}"),
            &[("X-Pinterest-PWS-Handler", "www/[username].js")],
        )
        .await?;
    pin_info(known, page, &answer["resource_response"]["data"])
        .ok_or_else(|| "Pinterest: no such pin".into())
}

fn pin_info(known: &SourceUrl, page: &str, pin: &Value) -> Option<SourceInfo> {
    let file = pin["images"]["orig"]["url"].as_str()?;
    let creator = &pin["native_creator"];
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![file.to_owned()];
    info.artist_name = creator["full_name"].as_str().map(str::to_owned);
    if let Some(name) = creator["username"].as_str() {
        info.artist_account = Some(name.to_owned());
        info.profile_urls
            .push(format!("https://www.pinterest.com/{name}/"));
    }
    info.profile_urls.extend(
        creator["domain_url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .map(str::to_owned),
    );
    info.tags = tags_named(
        strings(&pin["hashtags"], None)
            .iter()
            .map(|t| t.trim_start_matches('#')),
    );
    info.title = [&pin["title"], &pin["rich_metadata"]["title"]]
        .into_iter()
        .map(text_of)
        .find(|t| !t.is_empty())
        .unwrap_or_default();
    info.description = [&pin["description"], &pin["rich_metadata"]["description"]]
        .into_iter()
        .map(text_of)
        .find(|t| !t.trim().is_empty())
        .unwrap_or_default();
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins() {
        let page = "https://www.pinterest.com/pin/551409548144250908/";
        let known = moekura_core::sites::parse(page).unwrap();
        let pin = json!({
            "images": { "orig": { "url": "https://i.pinimg.com/originals/a7/7c/67/a.png" } },
            "native_creator": { "username": "uchihajake", "full_name": "Jake", "domain_url": "" },
            "title": "", "rich_metadata": { "title": "Hands" }, "description": " ", "hashtags": ["#art"]
        });
        let info = pin_info(&known, page, &pin).unwrap();
        assert_eq!(info.profile_urls, ["https://www.pinterest.com/uchihajake/"]);
        assert_eq!(info.title, "Hands");
        assert_eq!(info.tags[0].name, "art");
    }
}
