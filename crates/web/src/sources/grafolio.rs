//! Grafolio projects, from its API.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html_to_text, key, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = key(page.rsplit('/').next().unwrap_or_default())?;
    let project = http
        .json(
            &format!("https://grafolio.ogq.me/api/projects/single/{id}"),
            &[],
        )
        .await?;
    project_info(known, page, &project).ok_or_else(|| "Grafolio: no such project".into())
}

fn project_info(known: &SourceUrl, page: &str, project: &Value) -> Option<SourceInfo> {
    let path = project["mainImage"]["path"].as_str()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![format!("https://files.grafolio.ogq.me/{path}")];
    if let Some(name) = project["owner"]["nickname"].as_str() {
        info.artist_name = Some(name.to_owned());
        info.profile_urls
            .push(format!("https://grafolio.ogq.me/profile/{name}/projects"));
    }
    info.tags = tags_named(strings(&project["tags"], None));
    info.title = text_of(&project["title"]);
    info.description = project["contentBlocks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["contentType"] == "TEXT")
        .map(|b| html_to_text(&text_of(&b["text"]["html"])))
        .collect::<Vec<_>>()
        .join("\n");
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn projects() {
        let page = "https://grafolio.ogq.me/project/detail/ccb07e90bdce4a868737abfca5136413";
        let known = moekura_core::sites::parse(page).unwrap();
        let project = json!({
            "title": "Cat", "tags": ["cat"], "owner": { "nickname": "리니" },
            "mainImage": { "path": "real/07ac/IMAGE/ae3c.jpg" },
            "contentBlocks": [{ "contentType": "TEXT", "text": { "html": "<p>Hi</p>" } }]
        });
        let info = project_info(&known, page, &project).unwrap();
        assert_eq!(
            info.files,
            ["https://files.grafolio.ogq.me/real/07ac/IMAGE/ae3c.jpg"]
        );
        assert_eq!(info.description, "Hi");
    }
}
