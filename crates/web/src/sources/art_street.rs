//! ArtStreet (MediBang) pictures, from their pages, and books, from the
//! book API. Mature works need a login (the `MSID` cookie).

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, strings, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let mut info = page_info(known, page, &body);
    if page.contains("/book/") {
        let id = page
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        let book = http
            .json(
                &format!("https://medibang.com/api/book/fixedList2/{id}/?quality=pc"),
                &[],
            )
            .await?;
        info.files = book_files(&book);
    }
    if info.files.is_empty() && info.title.is_empty() {
        return Err("ArtStreet: no such work".into());
    }
    Ok(info)
}

fn book_files(book: &Value) -> Vec<String> {
    let mut files: Vec<String> = book["coverUrl"]
        .as_str()
        .map(str::to_owned)
        .into_iter()
        .collect();
    files.extend(strings(
        &book["chapterList"][0]["pageList"],
        Some("publicBgImage"),
    ));
    files
}

fn page_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    let images = html::find(body, "div", |t| t.has_class("pictureDetails__image"))
        .map(|(_, inner)| inner)
        .unwrap_or_default();
    info.files = html::tags(images, "img")
        .into_iter()
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .map(|url| {
            moekura_core::sites::parse(&url)
                .and_then(|u| u.file_url)
                .unwrap_or(url)
        })
        .collect();
    let class_text = |classes: &[&str]| {
        classes.iter().find_map(|class| {
            ["a", "span", "div", "h1", "p"].iter().find_map(|tag| {
                html::find(body, tag, |t| t.has_class(class)).map(|(_, inner)| html_to_text(inner))
            })
        })
    };
    info.artist_name = class_text(&["pictureDetails__authorName", "book_info-author-name"]);
    info.title = class_text(&["pictureDetails__titleLink", "name"])
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    info.description =
        class_text(&["pictureDetails__summaryOriginal", "summary-txt"]).unwrap_or_default();
    info.tags = tags_named(
        html::tags(body, "a")
            .into_iter()
            .filter(|t| t.has_class("tag"))
            .map(|t| html_to_text(html::inner(body, &t)))
            .collect::<Vec<_>>(),
    );
    info
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn pictures_and_books() {
        let page = "https://medibang.com/picture/4b2112261505098280008769655/";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div class="pictureDetails__image"><img src="https://dthezntil550i.cloudfront.net/4b/latest/4b2112261505098280008769655/1280_960/d5b22a94.png"></div>
            <a class="pictureDetails__titleLink">  A
              cat </a><span class="pictureDetails__authorName">Neko</span>
            <div class="cmn-tag"><a class="tag" href="https://medibang.com/t/cat">cat</a></div>"#;
        let info = page_info(&known, page, body);
        assert_eq!(
            info.files,
            [
                "https://dthezntil550i.cloudfront.net/4b/latest/4b2112261505098280008769655/d5b22a94.png"
            ]
        );
        assert_eq!(info.title, "A cat");
        assert_eq!(info.artist_name.as_deref(), Some("Neko"));
        assert_eq!(info.tags[0].name, "cat");
        let book = json!({ "coverUrl": "https://x/c.jpg", "chapterList": [{ "pageList": [{ "publicBgImage": "https://x/1.jpg" }] }] });
        assert_eq!(book_files(&book), ["https://x/c.jpg", "https://x/1.jpg"]);
    }
}
