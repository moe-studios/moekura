//! Dotpict works, from its API.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, id_of, number, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = number(page.rsplit('/').next().unwrap_or_default())?;
    let api = http
        .json(&format!("https://api.dotpicko.net/works/{id}/detail"), &[])
        .await?;
    work_info(known, page, &api["data"]["work"]).ok_or_else(|| "Dotpict: no such work".into())
}

fn work_info(known: &SourceUrl, page: &str, work: &Value) -> Option<SourceInfo> {
    let file = work["image_url"].as_str()?;
    let user = &work["user"];
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![file.to_owned()];
    info.artist_name = user["name"].as_str().map(|n| n.trim().to_owned());
    info.artist_account = user["account"]
        .as_str()
        .filter(|a| !a.is_empty())
        .map(str::to_owned);
    if let Some(id) = id_of(&user["id"]) {
        info.profile_urls
            .push(format!("https://dotpict.net/users/{id}"));
    }
    info.profile_urls
        .extend(user["share_url"].as_str().map(str::to_owned));
    info.tags = tags_named(strings(&work["tags"], None));
    info.title = text_of(&work["title"]);
    info.description = text_of(&work["text"]);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn works() {
        let page = "https://dotpict.net/works/4814277";
        let known = moekura_core::sites::parse(page).unwrap();
        let work = json!({
            "image_url": "https://img.dotpicko.net/work/2023/06/09/20/57/a.png", "title": "Cat", "text": "",
            "tags": ["cat"], "user": { "id": 2011866, "name": " Neko ", "account": "neko", "share_url": "https://dotpict.net/@neko" }
        });
        let info = work_info(&known, page, &work).unwrap();
        assert_eq!(
            info.profile_urls,
            [
                "https://dotpict.net/users/2011866",
                "https://dotpict.net/@neko"
            ]
        );
        assert_eq!(info.artist_name.as_deref(), Some("Neko"));
    }
}
