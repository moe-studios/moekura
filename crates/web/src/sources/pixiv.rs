//! Pixiv works: `pixiv.net/artworks/<id>` (with or without a language),
//! the old `member_illust.php?illust_id=<id>`, and files on `i.pximg.net`
//! (`<id>_p<page>.png`). Read from Pixiv's public web API; its files need
//! Pixiv as the `Referer`.

use serde_json::Value;
use url::Url;

use super::{Http, SourceInfo, SourceTag, html_to_text, text_of};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub id: u64,
    /// For a file link, its page of the work (0-based).
    pub page: usize,
}

const REFERER: &str = "https://www.pixiv.net/";

/// The work a Pixiv URL is about.
pub(super) fn target(url: &Url) -> Option<Target> {
    let host = url.host_str()?;
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    if host == "i.pximg.net" {
        // …/img/2026/01/31/12/00/00/123_p1.png, or _p1_master1200.jpg.
        let file = segments.last()?;
        let (id, rest) = file.split_once("_p")?;
        let page: usize = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        return Some(Target {
            id: id.parse().ok()?,
            page,
        });
    }
    if !matches!(host, "www.pixiv.net" | "pixiv.net") {
        return None;
    }
    let id = match segments.as_slice() {
        ["artworks", id] | [_, "artworks", id] => id.parse().ok()?,
        ["member_illust.php"] => url
            .query_pairs()
            .find(|(k, _)| k == "illust_id")?
            .1
            .parse()
            .ok()?,
        _ => return None,
    };
    Some(Target { id, page: 0 })
}

/// A work's details from `/ajax/illust/<id>`, its pages' files from
/// `/ajax/illust/<id>/pages` (when there are several), and an ugoira's
/// zip and frames from `/ajax/illust/<id>/ugoira_meta`.
pub(super) fn parse(
    work: &Value,
    pages: Option<&Value>,
    ugoira: Option<&Value>,
    target: &Target,
) -> Option<SourceInfo> {
    let body = &work["body"];
    let user_id = text_of(&body["userId"]);
    if user_id.is_empty() {
        return None;
    }
    let mut files: Vec<String> = pages
        .and_then(|p| p["body"].as_array())
        .map(|pages| {
            pages
                .iter()
                .filter_map(|p| p["urls"]["original"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if files.is_empty()
        && let Some(original) = body["urls"]["original"].as_str()
    {
        files.push(original.to_owned());
    }
    let ugoira_frames = ugoira.and_then(|meta| {
        let meta = &meta["body"];
        let zip = meta["originalSrc"]
            .as_str()
            .or_else(|| meta["src"].as_str())?;
        files = vec![zip.to_owned()];
        Some(
            meta["frames"]
                .as_array()?
                .iter()
                .filter_map(|f| {
                    Some((
                        f["file"].as_str()?.to_owned(),
                        u32::try_from(f["delay"].as_u64()?).ok()?,
                    ))
                })
                .collect(),
        )
    });
    if target.page > 0 && target.page < files.len() {
        let chosen = files.remove(target.page);
        files.insert(0, chosen);
    }
    let tags = body["tags"]["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|t| {
                    Some(SourceTag {
                        name: t["tag"].as_str()?.to_owned(),
                        translation: t["translation"]["en"].as_str().map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut profile_urls = vec![format!("https://www.pixiv.net/users/{user_id}")];
    if let Some(account) = body["userAccount"].as_str().filter(|a| !a.is_empty()) {
        profile_urls.push(format!("https://www.pixiv.net/stacc/{account}"));
    }
    Some(SourceInfo {
        site: moekura_core::sites::PIXIV.name,
        page_url: format!("https://www.pixiv.net/artworks/{}", target.id),
        files,
        headers: vec![("Referer", REFERER.to_owned())],
        artist_name: body["userName"].as_str().map(str::to_owned),
        artist_account: body["userAccount"]
            .as_str()
            .filter(|a| !a.is_empty())
            .map(str::to_owned),
        profile_urls,
        tags,
        title: text_of(&body["illustTitle"]),
        description: html_to_text(&text_of(&body["illustComment"])),
        ugoira_frames,
    })
}

pub(super) async fn fetch(http: &Http<'_>, target: &Target) -> Result<SourceInfo, String> {
    let headers = [("Referer", REFERER)];
    let work = http
        .json(
            &format!("https://www.pixiv.net/ajax/illust/{}", target.id),
            &headers,
        )
        .await?;
    if work["error"].as_bool() == Some(true) {
        return Err(format!("Pixiv: {}", text_of(&work["message"])));
    }
    // Logged out, Pixiv sometimes won't list the pages; the first is
    // still something.
    let pages = if work["body"]["pageCount"].as_u64().unwrap_or(1) > 1 {
        http.json(
            &format!("https://www.pixiv.net/ajax/illust/{}/pages", target.id),
            &headers,
        )
        .await
        .ok()
    } else {
        None
    };
    // Type 2 is an ugoira.
    let ugoira = if work["body"]["illustType"].as_u64() == Some(2) {
        Some(
            http.json(
                &format!(
                    "https://www.pixiv.net/ajax/illust/{}/ugoira_meta",
                    target.id
                ),
                &headers,
            )
            .await?,
        )
    } else {
        None
    };
    parse(&work, pages.as_ref(), ugoira.as_ref(), target)
        .ok_or_else(|| "Pixiv: no such work".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn urls() {
        let t = |u: &str| target(&Url::parse(u).unwrap());
        assert_eq!(
            t("https://www.pixiv.net/en/artworks/123"),
            Some(Target { id: 123, page: 0 })
        );
        assert_eq!(
            t("https://www.pixiv.net/member_illust.php?mode=medium&illust_id=77"),
            Some(Target { id: 77, page: 0 })
        );
        assert_eq!(
            t("https://i.pximg.net/img-original/img/2026/01/31/12/00/00/123_p2.png"),
            Some(Target { id: 123, page: 2 })
        );
        assert_eq!(t("https://www.pixiv.net/users/5"), None);
    }

    #[test]
    fn works() {
        let work = json!({ "error": false, "body": {
            "illustTitle": "猫", "illustComment": "新作です<br />よろしく",
            "userId": "5", "userName": "Neko", "userAccount": "nekoart", "pageCount": 2,
            "urls": { "original": "https://i.pximg.net/a_p0.png" },
            "tags": { "tags": [
                { "tag": "猫", "translation": { "en": "cat" } },
                { "tag": "オリジナル" }
            ] }
        }});
        let pages = json!({ "body": [
            { "urls": { "original": "https://i.pximg.net/a_p0.png" } },
            { "urls": { "original": "https://i.pximg.net/a_p1.png" } }
        ]});
        let info = parse(&work, Some(&pages), None, &Target { id: 9, page: 1 }).unwrap();
        assert_eq!(info.files[0], "https://i.pximg.net/a_p1.png");
        assert_eq!(info.page_url, "https://www.pixiv.net/artworks/9");
        assert_eq!(info.description, "新作です\nよろしく");
        assert_eq!(
            info.profile_urls,
            [
                "https://www.pixiv.net/users/5",
                "https://www.pixiv.net/stacc/nekoart"
            ]
        );
        assert_eq!(info.tags[0].translation.as_deref(), Some("cat"));
        assert_eq!(info.artist_name.as_deref(), Some("Neko"));
        assert_eq!(info.ugoira_frames, None);

        let meta = json!({ "body": {
            "originalSrc": "https://i.pximg.net/img-zip-ugoira/a_ugoira1920x1080.zip",
            "frames": [{ "file": "000000.jpg", "delay": 80 }, { "file": "000001.jpg", "delay": 120 }]
        }});
        let info = parse(&work, None, Some(&meta), &Target { id: 9, page: 0 }).unwrap();
        assert_eq!(
            info.files,
            ["https://i.pximg.net/img-zip-ugoira/a_ugoira1920x1080.zip"]
        );
        assert_eq!(
            info.ugoira_frames,
            Some(vec![
                ("000000.jpg".to_owned(), 80),
                ("000001.jpg".to_owned(), 120)
            ])
        );
    }
}
