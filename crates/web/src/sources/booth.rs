//! Booth items, from `<item>.json`: the item's images (originals), the
//! shop, tags and description.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let item = http
        .json(&format!("{page}.json"), &[("Cookie", "adult=t")])
        .await?;
    let mut info = item_info(known, page, &item).ok_or("Booth: no such item")?;
    // `_base_resized.jpg` images are samples; the original's extension
    // isn't said.
    let mut files = Vec::new();
    for file in &info.files {
        match file.split_once("_base_resized") {
            Some((stem, _)) => {
                let candidates: Vec<String> = ["png", "jpg", "jpeg"]
                    .iter()
                    .map(|ext| format!("{stem}.{ext}"))
                    .collect();
                files.push(
                    http.first_existing(&candidates, &[])
                        .await
                        .unwrap_or_else(|| file.clone()),
                );
            }
            None => files.push(file.clone()),
        }
    }
    info.files = files;
    Ok(info)
}

fn item_info(known: &SourceUrl, page: &str, item: &Value) -> Option<SourceInfo> {
    let shop = &item["shop"];
    let mut info = SourceInfo::new(known.site, page);
    info.files = strings(item.get("images")?, Some("original"));
    info.artist_name = shop["name"].as_str().map(str::to_owned);
    info.artist_account = shop["subdomain"].as_str().map(str::to_owned);
    if let Some(url) = shop["url"].as_str() {
        info.profile_urls.push(url.trim_end_matches('/').to_owned());
    }
    info.tags = tags_named(strings(&item["tags"], Some("name")));
    info.title = text_of(&item["name"]);
    info.description = text_of(&item["description"]);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn items() {
        let page = "https://booth.pm/en/items/2864768";
        let known = moekura_core::sites::parse(page).unwrap();
        let item = json!({
            "name": "Stickers", "description": "Cute", "tags": [{ "name": "cat" }],
            "shop": { "name": "Re:Face", "subdomain": "re-face", "url": "https://re-face.booth.pm/" },
            "images": [{ "original": "https://booth.pximg.net/u/i/2864768/a_base_resized.jpg" }]
        });
        let info = item_info(&known, page, &item).unwrap();
        assert_eq!(
            info.files,
            ["https://booth.pximg.net/u/i/2864768/a_base_resized.jpg"]
        );
        assert_eq!(info.profile_urls, ["https://re-face.booth.pm"]);
        assert_eq!(info.artist_account.as_deref(), Some("re-face"));
    }
}
