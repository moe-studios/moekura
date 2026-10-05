//! note posts, from its API.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html_to_text, key, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = key(page.rsplit('/').next().unwrap_or_default())?;
    let answer = http
        .json(&format!("https://note.com/api/v3/notes/{id}"), &[])
        .await?;
    note_info(known, page, &answer["data"]).ok_or_else(|| "note: no such post".into())
}

fn note_info(known: &SourceUrl, page: &str, note: &Value) -> Option<SourceInfo> {
    note.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    let mut files = strings(&note["pictures"], Some("url"));
    if note["type"] == "TextNote" {
        let body = text_of(&note["body"]);
        files.extend(
            super::html::tags(&body, "img")
                .into_iter()
                .filter_map(|t| t.attr("src").map(str::to_owned)),
        );
        info.description = html_to_text(&body);
    } else {
        info.description = strings(&note["pictures"], Some("caption")).join("\n\n");
    }
    info.files = files
        .into_iter()
        .map(|url| {
            moekura_core::sites::parse(&url)
                .and_then(|u| u.file_url)
                .unwrap_or(url)
        })
        .collect();
    let user = &note["user"];
    info.artist_name = user["nickname"].as_str().map(str::to_owned);
    if let Some(name) = user["urlname"].as_str() {
        info.artist_account = Some(name.to_owned());
        info.profile_urls.push(format!("https://note.com/{name}"));
    }
    info.tags = tags_named(
        note["hashtag_notes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|h| h["hashtag"]["name"].as_str())
            .map(|t| t.trim_start_matches('#')),
    );
    info.title = text_of(&note["name"]);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts() {
        let page = "https://note.com/koma_labo/n/n32fb90fac512";
        let known = moekura_core::sites::parse(page).unwrap();
        let note = json!({
            "type": "ImageNote", "name": "Cat", "user": { "urlname": "koma_labo", "nickname": "Koma" },
            "pictures": [{ "url": "https://assets.st-note.com/img/1623726537463-B8LOZ1JZUS.png?width=2000", "caption": "Hi" }],
            "hashtag_notes": [{ "hashtag": { "name": "#cat" } }]
        });
        let info = note_info(&known, page, &note).unwrap();
        assert_eq!(
            info.files,
            ["https://d2l930y2yx77uc.cloudfront.net/img/1623726537463-B8LOZ1JZUS.png"]
        );
        assert_eq!(info.tags[0].name, "cat");
        assert_eq!(info.description, "Hi");
    }
}
