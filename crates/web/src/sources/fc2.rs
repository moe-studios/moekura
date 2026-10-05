//! FC2 blog entries: the entry's images (`…s.jpg` thumbnails become their
//! originals), title and text, read as a phone with the age check passed.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, key, number};

const HEADERS: [(&str, &str); 2] = [("User-Agent", "Android Mobile"), ("Cookie", "age_check=1")];
/// The most of an entry's images kept, each asked for at its original
/// size: the entry is whoever wrote it's, and may list any number.
const MAX_IMAGES: usize = 30;

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    // An album's file may come from the link's query, where anything
    // could be.
    let url = url::Url::parse(page).map_err(|e| e.to_string())?;
    let path: Vec<&str> = url.path_segments().into_iter().flatten().collect();
    match path.as_slice() {
        [file] => {
            let entry = file
                .strip_prefix("blog-entry-")
                .and_then(|f| f.strip_suffix(".html"))
                .ok_or("FC2: not an entry")?;
            number(entry)?;
        }
        ["img", file, ""] => {
            key(file)?;
        }
        _ => return Err("FC2: not an entry".into()),
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("FC2: not an entry".into());
    }
    // Canonical entries are on http; blogs answer https too, which a
    // login needs.
    let secure = page.replacen("http://", "https://", 1);
    let body = match http.page(&secure, &HEADERS).await {
        Ok(body) => body,
        Err(_) => http.page(page, &HEADERS).await?,
    };
    let mut info = entry_info(known, page, &body).ok_or("FC2: no entry in the page")?;
    let mut files = Vec::new();
    for file in &info.files {
        let candidates = match file.strip_suffix("s.jpg") {
            Some(stem) => vec![format!("{stem}.jpg"), file.clone()],
            None => vec![file.clone()],
        };
        files.push(
            http.first_existing(&candidates, &[])
                .await
                .unwrap_or_else(|| file.clone()),
        );
    }
    info.files = files;
    Ok(info)
}

fn entry_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let (_, entry) = html::find(body, "div", |t| t.has_class("entry_body"))?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(entry, "img")
        .into_iter()
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .take(MAX_IMAGES)
        .collect();
    info.title = html::find(body, "div", |t| t.has_class("entry_title"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html_to_text(entry);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries() {
        let page = "http://hosystem.blog.fc2.com/blog-entry-37.html";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div class="entry_title"><h1><strong>Art</strong></h1></div>
            <div class="entry_body"><a href="x"><img src="https://blog-imgs-119.fc2.com/n/i/y/niyamalog/as.jpg"></a>Drawn</div>"#;
        let info = entry_info(&known, page, body).unwrap();
        assert_eq!(
            info.files,
            ["https://blog-imgs-119.fc2.com/n/i/y/niyamalog/as.jpg"]
        );
        assert_eq!(info.title, "Art");
        assert_eq!(info.description, "Drawn");

        // However many images the entry lists.
        let images = "<img src=\"https://blog-imgs-1.fc2.com/a/s.jpg\">".repeat(1000);
        let body = format!(r#"<div class="entry_body">{images}</div>"#);
        let info = entry_info(&known, page, &body).unwrap();
        assert_eq!(info.files.len(), MAX_IMAGES);
    }
}
