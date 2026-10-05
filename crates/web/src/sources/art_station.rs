//! ArtStation projects, from `/projects/<id>.json`: every image (at its
//! largest size that exists) and video, the artist, tags and text.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, key, strings, tags_named, text_of};

/// ArtStation answers crawlers it knows, not unknown programs.
const AGENT: (&str, &str) = (
    "User-Agent",
    "Mozilla/5.0 (compatible; Discordbot/2.0; +https://discordapp.com)",
);

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = key(page.rsplit('/').next().unwrap_or_default())?;
    let project = http
        .json(
            &format!("https://www.artstation.com/projects/{id}.json"),
            &[AGENT],
        )
        .await?;
    let mut info = project_info(known, page, &project).ok_or("ArtStation: no such project")?;
    // The `original` size isn't always there.
    let mut files = Vec::new();
    for file in &info.files {
        let sizes: Vec<String> = ["original", "4k", "large"]
            .iter()
            .map(|size| file.replacen("/original/", &format!("/{size}/"), 1))
            .collect();
        files.push(
            http.first_existing(&sizes, &[])
                .await
                .unwrap_or_else(|| file.clone()),
        );
    }
    for asset in project["assets"].as_array().into_iter().flatten() {
        if asset["asset_type"] == "video_clip"
            && let Some(player) = asset["player_embedded"].as_str()
            && let Some(src) = html::tags(player, "iframe")
                .first()
                .and_then(|t| t.attr("src").map(str::to_owned))
            && let Ok(body) = http.page(&src, &[AGENT]).await
            && let Some(video) = html::tags(&body, "source")
                .first()
                .and_then(|t| t.attr("src"))
        {
            files.push(video.to_owned());
        }
    }
    info.files = files;
    Ok(info)
}

fn project_info(known: &SourceUrl, page: &str, project: &Value) -> Option<SourceInfo> {
    let user = &project["user"];
    let name = user["username"].as_str()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = project["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["asset_type"] == "image")
        .filter_map(|a| a["image_url"].as_str())
        .map(|url| {
            moekura_core::sites::parse(url)
                .and_then(|u| u.file_url)
                .unwrap_or_else(|| url.to_owned())
        })
        .collect();
    info.artist_name = user["full_name"].as_str().map(str::to_owned);
    info.artist_account = Some(name.to_owned());
    info.profile_urls = vec![format!("https://www.artstation.com/{name}")];
    info.tags = tags_named(strings(&project["tags"], None));
    info.title = text_of(&project["title"]);
    info.description = html_to_text(&text_of(&project["description"]));
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn projects() {
        let page = "https://www.artstation.com/artwork/04XA4";
        let known = moekura_core::sites::parse(page).unwrap();
        let project = json!({
            "title": "Cat", "description": "<p>A cat</p>", "tags": ["cat"],
            "user": { "username": "sa-dui", "full_name": "Sa Dui" },
            "assets": [{ "asset_type": "image", "image_url": "https://cdna.artstation.com/p/assets/images/images/005/804/224/large/a.jpg?1493887236" }]
        });
        let info = project_info(&known, page, &project).unwrap();
        assert_eq!(
            info.files,
            [
                "https://cdn.artstation.com/p/assets/images/images/005/804/224/original/a.jpg?1493887236"
            ]
        );
        assert_eq!(info.profile_urls, ["https://www.artstation.com/sa-dui"]);
        assert_eq!(info.description, "A cat");
    }
}
