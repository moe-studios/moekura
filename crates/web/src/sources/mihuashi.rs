//! Mihuashi artworks, stalls, projects, character cards and activity
//! works, from its API.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, id_of, key, strings, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let path = page.trim_start_matches("https://www.mihuashi.com/");
    let path = path.split('?').next().unwrap_or(path);
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts {
        key(part)?;
    }
    let (api, key) = match parts.as_slice() {
        ["artworks", id] => (format!("v1/artworks/{id}/"), "artwork"),
        ["stalls", id] => (format!("v1/stalls/{id}/"), "stall"),
        ["projects", id] => (format!("v1/projects/{id}/"), "project"),
        ["character-card", id] => (format!("v1/character_cards/{id}/"), "character_card"),
        ["activities", activity, _, id] => (
            format!("activity/v1/activities/{activity}/artworks/{id}/"),
            "artwork",
        ),
        _ => return Err("Mihuashi: not a work".into()),
    };
    let answer = http
        .json(&format!("https://www.mihuashi.com/api/{api}"), &[])
        .await?;
    work_info(known, page, key, &answer[key]).ok_or_else(|| "Mihuashi: no such work".into())
}

fn original(url: &str) -> String {
    moekura_core::sites::parse(url)
        .and_then(|u| u.file_url)
        .unwrap_or_else(|| url.to_owned())
}

fn work_info(known: &SourceUrl, page: &str, key: &str, work: &Value) -> Option<SourceInfo> {
    work.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    let mut files: Vec<String> = Vec::new();
    match key {
        "artwork" => {
            files.extend(work["url"].as_str().map(str::to_owned));
            files.extend(strings(&work["images"], Some("url")));
        }
        "stall" => {
            files.extend(work["cover_url"].as_str().map(str::to_owned));
            files.extend(strings(&work["example_images"], None));
        }
        "character_card" => {
            files.extend(work["image_url"].as_str().map(str::to_owned));
            files.extend(strings(&work["example_images"], Some("url")));
        }
        _ => files.extend(strings(&work["example_images"], Some("url"))),
    }
    files.dedup();
    info.files = files.iter().map(|f| original(f)).collect();
    let user = [&work["author"], &work["owner"]]
        .into_iter()
        .find(|u| u.is_object())
        .cloned()
        .unwrap_or_default();
    info.artist_name = user["name"].as_str().map(str::to_owned);
    if let Some(id) = id_of(&user["id"]) {
        info.profile_urls
            .push(format!("https://www.mihuashi.com/profiles/{id}"));
    }
    info.tags = tags_named(strings(&work["tags"], Some("name")));
    info.title = [&work["name"], &work["title"]]
        .into_iter()
        .find_map(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    info.description = [
        &work["description"],
        &work["about"]["introduction"],
        &work["template"]["summary"],
        &work["summary"],
    ]
    .into_iter()
    .find_map(|v| v.as_str())
    .unwrap_or_default()
    .to_owned();
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn artworks() {
        let page = "https://www.mihuashi.com/artworks/15092919";
        let known = moekura_core::sites::parse(page).unwrap();
        let work = json!({
            "url": "https://image-assets.mihuashi.com/permanent/29105|-2024/05/29/16/FuE-9jWo-aPKXOq2KP2ZsR5Nxnqa.jpg!sq300",
            "description": "Cat", "tags": [{ "name": "cat" }], "author": { "id": 29105, "name": "Neko" }
        });
        let info = work_info(&known, page, "artwork", &work).unwrap();
        assert_eq!(
            info.files,
            [
                "https://image-assets.mihuashi.com/permanent/29105|-2024/05/29/16/FuE-9jWo-aPKXOq2KP2ZsR5Nxnqa.jpg"
            ]
        );
        assert_eq!(
            info.profile_urls,
            ["https://www.mihuashi.com/profiles/29105"]
        );
    }
}
