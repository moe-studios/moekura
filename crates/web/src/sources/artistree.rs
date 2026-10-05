//! Artistree commission listings, from Artifyc's API: the listing's
//! example images and text.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, key, strings, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let (profile, listing) = page.split_once('#').ok_or("Artistree: not a listing")?;
    let name = key(profile.rsplit('/').next().unwrap_or_default())?;
    let api = http
        .json(
            &format!("https://api.artifyc.com/commission/request?artist={name}"),
            &[],
        )
        .await?;
    listing_info(known, page, &api, listing).ok_or_else(|| "Artistree: no such listing".into())
}

fn listing_info(known: &SourceUrl, page: &str, api: &Value, listing: &str) -> Option<SourceInfo> {
    let commission = api["commission_info"]
        .as_array()?
        .iter()
        .find(|c| c["listing_id"].as_str() == Some(listing))?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = strings(&commission["reference_images"], None);
    info.title = text_of(&commission["name"]);
    info.description = text_of(&commission["additional_info"]);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn listings() {
        let page = "https://artistree.io/crestfallen163#d2ca";
        let known = moekura_core::sites::parse(page).unwrap();
        let api = json!({ "commission_info": [
            { "listing_id": "d2ca", "name": "Headshot", "additional_info": "Simple", "reference_images": ["https://x/a.png"] }
        ]});
        let info = listing_info(&known, page, &api, "d2ca").unwrap();
        assert_eq!(info.files, ["https://x/a.png"]);
        assert_eq!(info.title, "Headshot");
    }
}
