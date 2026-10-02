//! FC2 blog entries: the entry's images (`…s.jpg` thumbnails become their
//! originals), title and text, read as a phone with the age check passed.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text};

const HEADERS: [(&str, &str); 2] = [("User-Agent", "Android Mobile"), ("Cookie", "age_check=1")];

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &HEADERS).await?;
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
    }
}
