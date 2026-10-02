//! Posts on X (Twitter): `x.com/<user>/status/<id>` (or twitter.com,
//! with `/photo/<n>`). Read from the embed API that shows posts on other
//! sites, which needs no account; images are asked for at full size.

use serde_json::Value;
use url::Url;

use super::{Http, SourceInfo, SourceTag, text_of};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub user: String,
    pub id: u64,
    /// `/photo/<n>`'s picture, 0-based.
    pub page: usize,
}

pub(super) fn target(url: &Url) -> Option<Target> {
    let host = url
        .host_str()?
        .trim_start_matches("www.")
        .trim_start_matches("mobile.");
    if !matches!(
        host,
        "x.com" | "twitter.com" | "fxtwitter.com" | "vxtwitter.com" | "fixupx.com"
    ) {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [user, "status", id, rest @ ..] => Some(Target {
            user: (*user).to_owned(),
            id: id.parse().ok()?,
            page: match rest {
                ["photo", n, ..] => n.parse::<usize>().ok()?.saturating_sub(1),
                _ => 0,
            },
        }),
        _ => None,
    }
}

/// The embed API's token for a post: its id ÷ 10¹⁵ × π, in base 36,
/// without zeros and the point.
fn token(id: u64) -> String {
    let value = (id as f64 / 1e15) * std::f64::consts::PI;
    let mut whole = value.trunc() as u64;
    let mut fraction = value.fract();
    let digit = |d: u64| char::from_digit(d as u32, 36).unwrap_or('0');
    let mut int_part = String::new();
    loop {
        int_part.insert(0, digit(whole % 36));
        whole /= 36;
        if whole == 0 {
            break;
        }
    }
    let mut frac_part = String::new();
    for _ in 0..11 {
        fraction *= 36.0;
        let d = fraction.trunc() as u64;
        frac_part.push(digit(d));
        fraction -= d as f64;
    }
    format!("{int_part}{frac_part}").replace('0', "")
}

pub(super) fn parse(post: &Value, target: &Target) -> Option<SourceInfo> {
    let user = &post["user"];
    let screen_name = user["screen_name"].as_str()?.to_owned();
    let mut files: Vec<String> = post["mediaDetails"]
        .as_array()
        .map(|media| {
            media
                .iter()
                .filter_map(|m| match m["type"].as_str() {
                    Some("photo") => m["media_url_https"]
                        .as_str()
                        .map(|u| format!("{u}?name=orig")),
                    // The best MP4 of videos and GIFs.
                    _ => m["video_info"]["variants"]
                        .as_array()?
                        .iter()
                        .filter(|v| v["content_type"] == "video/mp4")
                        .max_by_key(|v| v["bitrate"].as_u64().unwrap_or(0))?["url"]
                        .as_str()
                        .map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    if target.page > 0 && target.page < files.len() {
        let chosen = files.remove(target.page);
        files.insert(0, chosen);
    }
    let tags = post["entities"]["hashtags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|t| {
                    Some(SourceTag {
                        name: t["text"].as_str()?.to_owned(),
                        translation: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut profile_urls = vec![format!("https://twitter.com/{screen_name}")];
    if let Some(id) = user["id_str"].as_str() {
        profile_urls.push(format!("https://twitter.com/intent/user?user_id={id}"));
    }
    // The text, without the link to the post's own media at the end.
    let text = text_of(&post["text"]);
    let text = text
        .rsplit_once(" https://t.co/")
        .filter(|(_, link)| !link.contains(' '))
        .map_or(text.as_str(), |(before, _)| before)
        .trim();
    Some(SourceInfo {
        site: moekura_core::sites::TWITTER.name,
        page_url: format!("https://twitter.com/{screen_name}/status/{}", target.id),
        files,
        headers: Vec::new(),
        artist_name: user["name"].as_str().map(str::to_owned),
        artist_account: Some(screen_name.clone()),
        profile_urls,
        tags,
        title: String::new(),
        description: super::decode_entities(text),
        ugoira_frames: None,
    })
}

pub(super) async fn fetch(http: &Http<'_>, target: &Target) -> Result<SourceInfo, String> {
    let url = format!(
        "https://cdn.syndication.twimg.com/tweet-result?id={}&token={}&lang=en",
        target.id,
        token(target.id)
    );
    let post = http.json(&url, &[]).await?;
    parse(&post, target).ok_or_else(|| "X: no such post, or it's hidden".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn urls() {
        let t = |u: &str| target(&Url::parse(u).unwrap());
        assert_eq!(
            t("https://x.com/artist/status/17/photo/2"),
            Some(Target {
                user: "artist".into(),
                id: 17,
                page: 1
            })
        );
        assert!(t("https://twitter.com/artist").is_none());
        assert!(t("https://example.com/a/status/1").is_none());
    }

    #[test]
    fn tokens_have_no_zeros() {
        let token = token(1_800_000_000_000_000_000);
        assert!(!token.is_empty() && !token.contains('0') && !token.contains('.'));
    }

    #[test]
    fn posts() {
        let post = json!({
            "text": "New drawing &amp; more #cat https://t.co/abc",
            "user": { "screen_name": "artist", "name": "Artist", "id_str": "42" },
            "entities": { "hashtags": [{ "text": "cat" }] },
            "mediaDetails": [
                { "type": "photo", "media_url_https": "https://pbs.twimg.com/media/a.jpg" },
                { "type": "photo", "media_url_https": "https://pbs.twimg.com/media/b.jpg" }
            ]
        });
        let info = parse(
            &post,
            &Target {
                user: "x".into(),
                id: 5,
                page: 1,
            },
        )
        .unwrap();
        assert_eq!(info.files[0], "https://pbs.twimg.com/media/b.jpg?name=orig");
        assert_eq!(info.description, "New drawing & more #cat");
        assert_eq!(info.page_url, "https://twitter.com/artist/status/5");
        assert_eq!(
            info.profile_urls[1],
            "https://twitter.com/intent/user?user_id=42"
        );
        assert_eq!(info.tags[0].name, "cat");
    }
}
