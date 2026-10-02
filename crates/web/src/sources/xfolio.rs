//! Xfolio works, from their pages: the full-size images, the creator,
//! tags and text. Most works need a login (the `xfolio_session` cookie).

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let info = work_info(known, page, &body);
    if info.files.is_empty() {
        return Err("Xfolio: no images in the page".into());
    }
    Ok(info)
}

fn work_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    // Links to the full-scale view name the image and work.
    info.files = html::tags(body, "a")
        .into_iter()
        .filter_map(|a| a.attr("href").map(str::to_owned))
        .filter_map(|href| {
            let url = url::Url::parse(&href).ok()?;
            if url.path() != "/fullscale_image" {
                return None;
            }
            let get = |k: &str| url.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
            let (image, work) = (get("image_id")?, get("work_id")?);
            Some(format!(
                "https://xfolio.jp/user_asset.php?id={image}&work_id={work}&work_image_id={image}&type=work_image"
            ))
        })
        .collect();
    info.files.dedup();
    info.artist_name = html::tags(body, "div")
        .into_iter()
        .find(|t| t.has_class("creatorInfo"))
        .and_then(|t| t.attr("data-creator-name").map(str::to_owned));
    info.title = html::find(body, "h1", |t| t.has_class("article--detailInfo__title"))
        .or_else(|| html::find(body, "div", |t| t.has_class("article--detailInfo__title")))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::find(body, "div", |t| t.has_class("richDescriptionText"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    let tags: Value = html::tags(body, "div")
        .into_iter()
        .find(|t| t.has_class("article--detailInfo__tags"))
        .and_then(|t| {
            t.attr("data-tags")
                .and_then(|d| serde_json::from_str(d).ok())
        })
        .unwrap_or_default();
    info.tags = tags_named(
        tags.as_array()
            .into_iter()
            .flatten()
            .map(|t| text_of(&t["name"])),
    );
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn works() {
        let page = "https://xfolio.jp/portfolio/ben1shoga/works/237599";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div class="creatorInfo" data-creator-name="Ben"></div>
            <div class="article__wrap_img"><a href="https://xfolio.jp/fullscale_image?image_id=1128032&work_id=237599"><img></a></div>
            <div class="article--detailInfo__tags" data-tags='[{"name": "cat", "link": "x"}]'></div>"#;
        let info = work_info(&known, page, body);
        assert_eq!(
            info.files,
            [
                "https://xfolio.jp/user_asset.php?id=1128032&work_id=237599&work_image_id=1128032&type=work_image"
            ]
        );
        assert_eq!(info.artist_name.as_deref(), Some("Ben"));
        assert_eq!(info.tags[0].name, "cat");
    }
}
