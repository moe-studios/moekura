//! Carrd sections (`<site>/#<section>`): the section's images and videos
//! and its text.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let (site, section) = page.split_once("/#").ok_or("Carrd: not a section")?;
    let body = http.page(site, &[]).await?;
    let mut info =
        section_info(known, page, site, section, &body).ok_or("Carrd: no such section")?;
    // A gallery's image may have a larger `_original`.
    let mut files = Vec::new();
    for file in &info.files {
        let candidates = match file.rsplit_once('.') {
            Some((stem, ext)) if !stem.ends_with("_original") && ext != "mp4" => {
                vec![format!("{stem}_original.{ext}"), file.clone()]
            }
            _ => vec![file.clone()],
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

fn section_info(
    known: &SourceUrl,
    page: &str,
    site: &str,
    section: &str,
    body: &str,
) -> Option<SourceInfo> {
    let id = format!("{section}-section");
    let (_, content) = html::find(body, "section", |t| t.attr("id") == Some(id.as_str()))?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(content, "img")
        .into_iter()
        .chain(html::tags(content, "video"))
        .chain(html::tags(content, "source"))
        .filter_map(|t| {
            [t.attr("data-src"), t.attr("src")]
                .into_iter()
                .flatten()
                .find(|src| src.starts_with("assets"))
                .map(|src| format!("{site}/{}", src.split('?').next().unwrap_or(src)))
        })
        .collect();
    info.files.dedup();
    info.description = html_to_text(content);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections() {
        let page = "https://caminukai-art.carrd.co/#fanart";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<section id="fanart-section"><h2>Fan art</h2><img src="assets/images/gallery12/690db30b.jpg?v=3"></section>"#;
        let info = section_info(
            &known,
            page,
            "https://caminukai-art.carrd.co",
            "fanart",
            body,
        )
        .unwrap();
        assert_eq!(
            info.files,
            ["https://caminukai-art.carrd.co/assets/images/gallery12/690db30b.jpg"]
        );
        assert_eq!(info.description, "Fan art");
    }
}
