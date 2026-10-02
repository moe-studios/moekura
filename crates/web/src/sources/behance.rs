//! Behance projects, from the state JSON in their pages: every image
//! (largest size), the creator, tags and text.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[("Cookie", "ilo0=true")]).await?;
    let state = html::script_json(&body, "beconfig-store_state")
        .ok_or("Behance: no project in the page")?;
    project_info(known, page, &state["project"]["project"])
        .ok_or_else(|| "Behance: no such project".into())
}

/// The largest of a module's images.
fn largest(sizes: &Value) -> Option<String> {
    let url =
        sizes["allAvailable"]
            .as_array()?
            .iter()
            .max_by_key(|i| i["width"].as_u64().unwrap_or(0) * i["height"].as_u64().unwrap_or(1))?
            ["url"]
            .as_str()?;
    Some(
        moekura_core::sites::parse(url)
            .and_then(|u| u.file_url)
            .unwrap_or_else(|| url.to_owned()),
    )
}

fn project_info(known: &SourceUrl, page: &str, project: &Value) -> Option<SourceInfo> {
    project.as_object()?;
    let mut info = SourceInfo::new(known.site, project["url"].as_str().unwrap_or(page));
    let mut text = vec![text_of(&project["description"])];
    for module in project["modules"].as_array().into_iter().flatten() {
        match module["__typename"].as_str() {
            Some("ImageModule") => info.files.extend(largest(&module["imageSizes"])),
            Some("MediaCollectionModule") => info.files.extend(
                module["components"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| largest(&c["imageSizes"])),
            ),
            Some("TextModule") => text.push(html_to_text(&text_of(&module["text"]))),
            _ => {}
        }
    }
    let creator = &project["creator"];
    info.artist_name = creator["displayName"].as_str().map(str::to_owned);
    if let Some(url) = creator["url"].as_str() {
        info.artist_account = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(str::to_owned);
        info.profile_urls.push(url.to_owned());
    }
    info.tags = tags_named(strings(&project["tags"], Some("title")));
    info.title = text_of(&project["name"]);
    info.description = text
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn projects() {
        let page = "https://www.behance.net/gallery/97612065/SailorMoon";
        let known = moekura_core::sites::parse(page).unwrap();
        let project = json!({
            "name": "Sailor Moon", "description": "Fan art", "url": page,
            "creator": { "displayName": "Kensuke", "url": "https://www.behance.net/Kensukecreations" },
            "tags": [{ "title": "anime" }],
            "modules": [
                { "__typename": "ImageModule", "imageSizes": { "allAvailable": [
                    { "url": "https://mir-s3-cdn-cf.behance.net/project_modules/disp/ea4c7e97612065.5ec92bae8dc45.jpg", "width": 600, "height": 800 },
                    { "url": "https://mir-s3-cdn-cf.behance.net/project_modules/1400/ea4c7e97612065.5ec92bae8dc45.jpg", "width": 1400, "height": 1800 }
                ] } },
                { "__typename": "TextModule", "text": "<div>Thanks</div>" }
            ]
        });
        let info = project_info(&known, page, &project).unwrap();
        assert_eq!(
            info.files,
            [
                "https://mir-s3-cdn-cf.behance.net/project_modules/source/ea4c7e97612065.5ec92bae8dc45.jpg"
            ]
        );
        assert_eq!(info.artist_account.as_deref(), Some("Kensukecreations"));
        assert_eq!(info.description, "Fan art\n\nThanks");
    }
}
