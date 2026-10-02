//! Gumroad products and posts, from the props the page embeds.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, strings, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let props = html::tags(&body, "div")
        .into_iter()
        .find(|t| t.attr("id") == Some("app"))
        .and_then(|t| {
            t.attr("data-page")
                .and_then(|d| serde_json::from_str::<Value>(d).ok())
        })
        .map(|page| page["props"].clone())
        .ok_or("Gumroad: nothing in the page")?;
    props_info(known, page, &props).ok_or_else(|| "Gumroad: no such product".into())
}

fn props_info(known: &SourceUrl, page: &str, props: &Value) -> Option<SourceInfo> {
    let mut info = SourceInfo::new(known.site, page);
    let product = &props["product"];
    let account = |url: &str| moekura_core::sites::parse(url).and_then(|u| u.profile_url);
    if product.is_object() {
        info.files = strings(&product["covers"], Some("original_url"));
        let seller = &product["seller"];
        info.artist_name = seller["name"].as_str().map(str::to_owned);
        info.profile_urls
            .extend(seller["profile_url"].as_str().and_then(account));
        info.title = text_of(&product["name"]).trim().to_owned();
        info.description = html_to_text(&text_of(&product["description_html"]));
    } else {
        let message = props["message"].as_str()?;
        info.files = html::tags(message, "img")
            .into_iter()
            .filter_map(|t| t.attr("src").map(str::to_owned))
            .filter(|src| src.contains("gumroad.com"))
            .collect();
        let creator = &props["creator_profile"];
        info.artist_name = creator["name"].as_str().map(str::to_owned);
        if let Some(sub) = creator["subdomain"].as_str() {
            info.profile_urls
                .push(format!("https://{}", sub.trim_end_matches('/')));
        }
        info.description = html_to_text(message);
    }
    info.artist_account = info
        .profile_urls
        .first()
        .and_then(|u| u.strip_prefix("https://"))
        .and_then(|h| h.strip_suffix(".gumroad.com"))
        .map(str::to_owned);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn products() {
        let page = "https://aiki.gumroad.com/l/HelmV2T3";
        let known = moekura_core::sites::parse(page).unwrap();
        let props = json!({ "product": {
            "name": " Helm ", "description_html": "<p>3D</p>",
            "covers": [{ "original_url": "https://public-files.gumroad.com/zc2289" }],
            "seller": { "name": "Aiki", "profile_url": "https://aiki.gumroad.com" }
        }});
        let info = props_info(&known, page, &props).unwrap();
        assert_eq!(info.files, ["https://public-files.gumroad.com/zc2289"]);
        assert_eq!(info.profile_urls, ["https://aiki.gumroad.com"]);
        assert_eq!(info.artist_account.as_deref(), Some("aiki"));
        assert_eq!(info.title, "Helm");
    }
}
