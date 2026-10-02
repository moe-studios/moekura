//! Fandom wiki files, from the wiki's Lightbox API: the file, who
//! uploaded it, and its description.

use moekura_core::sites::SourceUrl;
use serde_json::Value;
use url::Url;

use super::{Http, SourceInfo, html_to_text, text_of};

/// The wiki (`https://<wiki>.fandom.com[/<lang>]`) and file a page names.
fn wiki_and_file(page: &str) -> Option<(String, String)> {
    let url = Url::parse(page).ok()?;
    let file = url
        .query_pairs()
        .find(|(k, _)| k == "file")
        .map(|(_, v)| v.into_owned())
        .or_else(|| {
            let path = percent_encoding::percent_decode_str(url.path())
                .decode_utf8_lossy()
                .into_owned();
            path.split_once("/wiki/File:").map(|(_, f)| f.to_owned())
        })?;
    let wiki = page.split_once("/wiki/")?.0.to_owned();
    Some((wiki, file))
}

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let (wiki, file) = wiki_and_file(page).ok_or("Fandom: not a file")?;
    let title: String = url::form_urlencoded::byte_serialize(file.as_bytes()).collect();
    let detail = http
        .json(
            &format!(
                "{wiki}/wikia.php?controller=Lightbox&method=getMediaDetail&fileTitle={title}"
            ),
            &[],
        )
        .await?;
    media_info(known, page, &wiki, &detail).ok_or_else(|| "Fandom: no such file".into())
}

fn media_info(known: &SourceUrl, page: &str, wiki: &str, detail: &Value) -> Option<SourceInfo> {
    let raw = detail["rawImageUrl"]
        .as_str()
        .filter(|_| detail["mediaType"] == "image")?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![
        moekura_core::sites::parse(raw)
            .and_then(|u| u.file_url)
            .unwrap_or_else(|| raw.to_owned()),
    ];
    info.artist_name = detail["userName"]
        .as_str()
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    info.profile_urls = detail["userPageUrl"]
        .as_str()
        .filter(|u| !u.is_empty())
        .map_or_else(|| vec![wiki.to_owned()], |u| vec![u.to_owned()]);
    info.description = html_to_text(&text_of(&detail["imageDescription"]));
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn files() {
        let page = "https://typemoon.fandom.com/wiki/Tamamo?file=Caster%5FDesign.png";
        assert_eq!(
            wiki_and_file(page),
            Some((
                "https://typemoon.fandom.com".into(),
                "Caster_Design.png".into()
            ))
        );
        let known = moekura_core::sites::parse(page).unwrap();
        let detail = json!({
            "mediaType": "image", "rawImageUrl": "https://static.wikia.nocookie.net/typemoon/images/3/3f/Caster_Design.png/revision/latest?cb=1",
            "userName": "Fan", "userPageUrl": "https://typemoon.fandom.com/wiki/User:Fan", "imageDescription": "<p>Art</p>"
        });
        let info = media_info(&known, page, "https://typemoon.fandom.com", &detail).unwrap();
        assert_eq!(
            info.profile_urls,
            ["https://typemoon.fandom.com/wiki/User:Fan"]
        );
        assert_eq!(info.description, "Art");
    }
}
