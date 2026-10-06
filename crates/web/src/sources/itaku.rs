//! Itaku images, posts and commissions, from its API.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, number, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let path = page.trim_start_matches("https://itaku.ee/");
    let api = match path.split_once('/') {
        Some(("images", id)) => format!("https://itaku.ee/api/galleries/images/{}/", number(id)?),
        Some(("posts", id)) => format!("https://itaku.ee/api/posts/{}/", number(id)?),
        Some(("commissions", id)) => {
            format!("https://itaku.ee/api/commissions/{}/", number(id)?)
        }
        _ => return Err("Itaku: not a work".into()),
    };
    let answer = http.json(&api, &[]).await?;
    work_info(known, page, &answer).ok_or_else(|| "Itaku: no such work".into())
}

fn work_info(known: &SourceUrl, page: &str, work: &Value) -> Option<SourceInfo> {
    let name = work["owner_username"].as_str()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = if page.contains("/posts/") {
        strings(&work["gallery_images"], Some("image"))
    } else if page.contains("/commissions/") {
        let mut files = strings(&work["reference_gallery_images"], Some("image"));
        files.extend(strings(
            &work["finished_work_gallery_images"],
            Some("image"),
        ));
        files
    } else {
        work["video"]["video"]
            .as_str()
            .or_else(|| work["image"].as_str())
            .map(str::to_owned)
            .into_iter()
            .collect()
    };
    info.artist_name = work["owner_displayname"].as_str().map(str::to_owned);
    info.artist_account = Some(name.to_owned());
    info.profile_urls
        .push(format!("https://itaku.ee/profile/{name}"));
    info.tags = tags_named(strings(&work["tags"], Some("name")));
    info.title = text_of(&work["title"]);
    info.description = work["description"]
        .as_str()
        .or_else(|| work["content"].as_str())
        .unwrap_or_default()
        .to_owned();
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn images_and_posts() {
        let page = "https://itaku.ee/images/812661";
        let known = moekura_core::sites::parse(page).unwrap();
        let image = json!({
            "owner_username": "advosart", "owner_displayname": "Advos", "title": "Cat", "description": "Hi",
            "image": "https://itaku.ee/api/media/gallery_imgs/IMG_2679_3GtFUgB.png", "tags": [{ "name": "cat" }]
        });
        let info = work_info(&known, page, &image).unwrap();
        assert_eq!(
            info.files,
            ["https://itaku.ee/api/media/gallery_imgs/IMG_2679_3GtFUgB.png"]
        );
        assert_eq!(info.profile_urls, ["https://itaku.ee/profile/advosart"]);
        let page = "https://itaku.ee/posts/130073";
        let post = json!({ "owner_username": "advosart", "gallery_images": [{ "image": "https://x/1.png" }, { "image": "https://x/2.png" }] });
        assert_eq!(work_info(&known, page, &post).unwrap().files.len(), 2);
    }
}
