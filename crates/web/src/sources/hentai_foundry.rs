//! Hentai Foundry pictures, from their pages (past the age check).

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, key, number, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    check_page(page)?;
    let body = http.page(&format!("{page}?enterAgree=1"), &[]).await?;
    picture_info(known, page, &body).ok_or_else(|| "Hentai Foundry: no picture in the page".into())
}

/// Refuses a picture's page unless it's a user's name and a number (a
/// thumbnail's link names the picture in its query, where anything could
/// be).
fn check_page(page: &str) -> Result<(), String> {
    let path = page
        .strip_prefix("https://www.hentai-foundry.com/")
        .ok_or("Hentai Foundry: not a picture")?;
    match path.split('/').collect::<Vec<_>>().as_slice() {
        ["pictures", "user", user, id] => key(user).and(number(id)).map(|_| ()),
        [pic] => number(pic.strip_prefix("pic-").unwrap_or_default()).map(|_| ()),
        _ => Err("Hentai Foundry: not a picture".into()),
    }
}

fn picture_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let (_, picture) = html::find(body, "div", |t| t.attr("id") == Some("picBox"))?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(picture, "img")
        .into_iter()
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .map(|src| {
            if src.starts_with("//") {
                format!("https:{src}")
            } else {
                src
            }
        })
        .collect();
    info.title = html::find(picture, "span", |t| t.has_class("imageTitle"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.tags = tags_named(
        html::tags(body, "a")
            .into_iter()
            .filter(|a| a.attr("rel") == Some("tag"))
            .filter_map(|a| html::label(body, &a))
            .collect::<Vec<_>>(),
    );
    info.description = html::find(body, "div", |t| t.has_class("picDescript"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures() {
        let page = "https://www.hentai-foundry.com/pictures/user/Afrobull/795025";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div id="picBox" class="box"><div class="boxheader"><span class="imageTitle">Kuroeda</span></div>
            <div class="boxbody"><img src="//pictures.hentai-foundry.com/a/Afrobull/795025/Afrobull-795025-kuroeda.png"></div></div>
            <div id="descriptionBox"><div class="picDescript">Commission</div></div>
            <a rel="tag" href="/pictures/tagged/cat">cat</a>"#;
        let info = picture_info(&known, page, body).unwrap();
        assert_eq!(
            info.files,
            ["https://pictures.hentai-foundry.com/a/Afrobull/795025/Afrobull-795025-kuroeda.png"]
        );
        assert_eq!(info.title, "Kuroeda");
        assert_eq!(info.description, "Commission");
    }
}
