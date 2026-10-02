//! Misskey notes (misskey.io, .art, .design, and other instances) and
//! Mastodon statuses (Pawoo, Baraag), from their public APIs.

use moekura_core::sites::{MISSKEY, SourceUrl};
use serde_json::{Value, json};
use url::Url;

use super::{Http, SourceInfo, SourceTag, html_to_text, text_of};

/// `:emoji:` codes out of a display name.
fn without_emoji(name: &str) -> String {
    let mut out = String::new();
    let mut rest = name;
    while let Some(start) = rest.find(':') {
        let after = &rest[start + 1..];
        match after.find(':') {
            Some(end)
                if end > 0
                    && after[..end].chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '.' | '-')
                    }) =>
            {
                out.push_str(&rest[..start]);
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str(&rest[..=start]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A note at `origin/notes/<id>`.
pub(super) async fn misskey(
    http: &Http<'_>,
    site: &'static moekura_core::sites::Site,
    page: &str,
) -> Result<SourceInfo, String> {
    let url = Url::parse(page).map_err(|e| e.to_string())?;
    let origin = url.origin().ascii_serialization();
    let id = page.rsplit('/').next().unwrap_or_default();
    let note = http
        .post_json(
            &format!("{origin}/api/notes/show"),
            &[],
            &json!({ "noteId": id }),
        )
        .await?;
    misskey_note(site, &origin, page, &note).ok_or_else(|| "Misskey: no such note".into())
}

fn misskey_note(
    site: &'static moekura_core::sites::Site,
    origin: &str,
    page: &str,
    note: &Value,
) -> Option<SourceInfo> {
    let user = &note["user"];
    let name = user["username"].as_str()?;
    let mut info = SourceInfo::new(site, page);
    let files = note["files"].as_array().cloned().unwrap_or_default();
    info.files = files
        .iter()
        .filter_map(|f| f["url"].as_str().map(str::to_owned))
        .collect();
    info.artist_name = user["name"]
        .as_str()
        .map(without_emoji)
        .filter(|n| !n.is_empty());
    info.artist_account = Some(name.to_owned());
    info.published_at = note["createdAt"].as_str().and_then(super::date);
    info.updated_at = note["updatedAt"].as_str().and_then(super::date);
    // A note from another instance names it.
    match user["host"].as_str() {
        Some(host) => {
            info.profile_urls.push(format!("{origin}/@{name}@{host}"));
            info.profile_urls.push(format!("https://{host}/@{name}"));
        }
        None => {
            info.profile_urls.push(format!("{origin}/@{name}"));
            if let Some(id) = user["id"].as_str() {
                info.profile_urls.push(format!("{origin}/users/{id}"));
            }
        }
    }
    info.tags = note["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|t| t.as_str())
                .map(|t| SourceTag {
                    name: t.to_owned(),
                    translation: None,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut text: Vec<String> = Vec::new();
    text.extend(note["cw"].as_str().map(str::to_owned));
    text.extend(note["text"].as_str().map(str::to_owned));
    text.extend(
        files
            .iter()
            .filter_map(|f| f["comment"].as_str().map(str::to_owned)),
    );
    info.description = text.join("\n");
    Some(info)
}

/// A note on an instance we don't know by name, if it answers like
/// Misskey.
pub(super) async fn other_misskey(http: &Http<'_>, url: &Url) -> Option<SourceInfo> {
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let ["notes", id] = segments.as_slice() else {
        return None;
    };
    let page = format!("{}/notes/{id}", url.origin().ascii_serialization());
    misskey(http, &MISSKEY, &page).await.ok()
}

/// A Mastodon status, from `/api/v1/statuses/<id>`.
pub(super) async fn mastodon(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let url = Url::parse(page).map_err(|e| e.to_string())?;
    let origin = url.origin().ascii_serialization();
    let id = page.rsplit('/').next().unwrap_or_default();
    let status = http
        .json(&format!("{origin}/api/v1/statuses/{id}"), &[])
        .await?;
    mastodon_status(known, &origin, &status).ok_or_else(|| "Mastodon: no such status".into())
}

fn mastodon_status(known: &SourceUrl, origin: &str, status: &Value) -> Option<SourceInfo> {
    let account = &status["account"];
    let name = account["username"].as_str()?;
    let id = text_of(&status["id"]);
    let mut info = SourceInfo::new(known.site, format!("{origin}/@{name}/{id}"));
    info.files = status["media_attachments"]
        .as_array()
        .map(|media| {
            media
                .iter()
                .filter_map(|m| m["url"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    info.artist_name = account["display_name"]
        .as_str()
        .map(without_emoji)
        .filter(|n| !n.is_empty());
    info.artist_account = Some(name.to_owned());
    info.published_at = status["created_at"].as_str().and_then(super::date);
    info.updated_at = status["edited_at"].as_str().and_then(super::date);
    info.profile_urls.push(format!("{origin}/@{name}"));
    if let Some(id) = account["id"].as_str() {
        info.profile_urls
            .push(format!("{origin}/web/accounts/{id}"));
    }
    info.tags = status["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|t| t["name"].as_str())
                .map(|t| SourceTag {
                    name: t.to_owned(),
                    translation: None,
                })
                .collect()
        })
        .unwrap_or_default();
    let spoiler = text_of(&status["spoiler_text"]);
    let content = html_to_text(&text_of(&status["content"]));
    info.description = [spoiler, content]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Some(info)
}

#[cfg(test)]
mod tests {
    use moekura_core::sites::MISSKEY_IO;

    use super::*;

    #[test]
    fn misskey_notes() {
        let note = json!({
            "text": "New #art", "tags": ["art"],
            "user": { "id": "9b", "username": "ixy", "name": "Ixy :heart:", "host": null },
            "files": [{ "url": "https://media.misskeyusercontent.jp/io/a.png", "comment": "alt" }]
        });
        let page = "https://misskey.io/notes/9bxaf592x6";
        let info = misskey_note(&MISSKEY_IO, "https://misskey.io", page, &note).unwrap();
        assert_eq!(info.files, ["https://media.misskeyusercontent.jp/io/a.png"]);
        assert_eq!(info.artist_name.as_deref(), Some("Ixy"));
        assert_eq!(
            info.profile_urls,
            ["https://misskey.io/@ixy", "https://misskey.io/users/9b"]
        );
        assert_eq!(info.description, "New #art\nalt");
    }

    #[test]
    fn mastodon_statuses() {
        let known =
            moekura_core::sites::parse("https://baraag.net/@curator/102270656480174153").unwrap();
        let status = json!({
            "id": "102270656480174153", "content": "<p>Hello<br>world</p>", "spoiler_text": "",
            "account": { "id": "1", "username": "curator", "display_name": "Curator" },
            "media_attachments": [{ "url": "https://media.baraag.net/a.png" }],
            "tags": [{ "name": "art" }]
        });
        let info = mastodon_status(&known, "https://baraag.net", &status).unwrap();
        assert_eq!(
            info.page_url,
            "https://baraag.net/@curator/102270656480174153"
        );
        assert_eq!(info.files, ["https://media.baraag.net/a.png"]);
        assert_eq!(info.description, "Hello\nworld");
        assert_eq!(info.profile_urls[1], "https://baraag.net/web/accounts/1");
    }
}
