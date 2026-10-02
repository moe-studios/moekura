//! OpenSea items, from the data their pages rehydrate.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let item = item_of(&body).ok_or("OpenSea: no item in the page")?;
    item_info(known, page, &item).ok_or_else(|| "OpenSea: no such item".into())
}

/// `….push({"rehydrate": {"…": {"data": {"itemByIdentifier": {…}}}}})`.
fn item_of(body: &str) -> Option<Value> {
    let at = body.find("itemByIdentifier")?;
    let start = body[..at].rfind(".push(")?;
    let data = html::json_after(&body[start..], ".push(")?;
    data["rehydrate"]
        .as_object()?
        .values()
        .find_map(|v| v["data"]["itemByIdentifier"].as_object().cloned())
        .map(Value::Object)
}

fn item_info(known: &SourceUrl, page: &str, item: &Value) -> Option<SourceInfo> {
    let file = item["animationUrl"]
        .as_str()
        .or_else(|| item["imageUrl"].as_str())?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![
        moekura_core::sites::parse(file)
            .and_then(|u| u.file_url)
            .unwrap_or_else(|| file.to_owned()),
    ];
    let owner = &item["collection"]["owner"];
    if let Some(name) = owner["displayName"].as_str() {
        info.artist_name = Some(name.to_owned());
        info.profile_urls.push(format!("https://opensea.io/{name}"));
    }
    if let Some(address) = owner["address"].as_str() {
        info.profile_urls
            .push(format!("https://opensea.io/{address}"));
    }
    info.title = text_of(&item["name"]);
    info.description = text_of(&item["description"]);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items() {
        let page = "https://opensea.io/item/matic/0x29/7";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<script>(window.__wired = []).push({"rehydrate": {"q": {"data": {"itemByIdentifier": {
            "name": "Cat #7", "description": "A cat", "imageUrl": "https://i2c.seadn.io/ethereum/0x49/61c6/9361.jpeg?w=1000",
            "collection": {"owner": {"displayName": "tororo", "address": "0x7C01"}}}}}}})</script>"#;
        let item = item_of(body).unwrap();
        let info = item_info(&known, page, &item).unwrap();
        assert_eq!(
            info.files,
            ["https://raw2.seadn.io/ethereum/0x49/61c6/9361.jpeg"]
        );
        assert_eq!(
            info.profile_urls,
            ["https://opensea.io/tororo", "https://opensea.io/0x7C01"]
        );
    }
}
