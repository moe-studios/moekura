//! Skeb commissions: `skeb.jp/@<creator>/works/<n>`, read from Skeb's web
//! API. The request text (the commissioner's) is the description.

use serde_json::Value;
use url::Url;

use super::{Http, SourceInfo, text_of};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub creator: String,
    pub work: u64,
}

pub(super) fn target(url: &Url) -> Option<Target> {
    if url.host_str()? != "skeb.jp" {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [creator, "works", n] => Some(Target {
            creator: creator
                .strip_prefix('@')
                .filter(|c| super::key(c).is_ok())?
                .to_owned(),
            work: n.parse().ok()?,
        }),
        _ => None,
    }
}

pub(super) fn parse(work: &Value, target: &Target) -> Option<SourceInfo> {
    let creator = &work["creator"];
    let name = creator["screen_name"]
        .as_str()
        .unwrap_or(&target.creator)
        .to_owned();
    let files = work["previews"]
        .as_array()?
        .iter()
        .filter_map(|p| p["url"].as_str().map(str::to_owned))
        .collect();
    Some(SourceInfo {
        site: moekura_core::sites::SKEB.name,
        page_url: format!("https://skeb.jp/@{name}/works/{}", target.work),
        files,
        headers: Vec::new(),
        artist_name: creator["name"].as_str().map(str::to_owned),
        artist_account: Some(name.clone()),
        profile_urls: vec![format!("https://skeb.jp/@{name}")],
        tags: Vec::new(),
        title: String::new(),
        description: text_of(&work["body"]),
        ugoira_frames: None,
        published_at: work["created_at"].as_str().and_then(super::date),
        updated_at: work["updated_at"].as_str().and_then(super::date),
    })
}

pub(super) async fn fetch(http: &Http<'_>, target: &Target) -> Result<SourceInfo, String> {
    let work = http
        .json(
            &format!(
                "https://skeb.jp/api/users/{}/works/{}",
                target.creator, target.work
            ),
            &[
                ("Authorization", "Bearer null"),
                ("Referer", "https://skeb.jp/"),
            ],
        )
        .await?;
    parse(&work, target).ok_or_else(|| "Skeb: no such work".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn works() {
        let url = Url::parse("https://skeb.jp/@neko/works/3").unwrap();
        let target = target(&url).unwrap();
        let work = json!({
            "body": "Please draw a cat",
            "creator": { "screen_name": "neko", "name": "Neko" },
            "previews": [{ "url": "https://si.skeb.jp/a.png" }]
        });
        let info = parse(&work, &target).unwrap();
        assert_eq!(info.files, ["https://si.skeb.jp/a.png"]);
        assert_eq!(info.profile_urls, ["https://skeb.jp/@neko"]);
    }
}
