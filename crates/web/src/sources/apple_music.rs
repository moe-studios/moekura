//! Apple Music albums: the cover, at its largest.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    page_info(known, page, &body).ok_or_else(|| "Apple Music: no cover".into())
}

fn page_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let srcset = html::tags(body, "source")
        .into_iter()
        .find_map(|t| t.attr("srcset").map(str::to_owned))?;
    let first = srcset.split(',').next()?.split_whitespace().next()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![
        moekura_core::sites::parse(first)
            .and_then(|u| u.file_url)
            .unwrap_or_else(|| first.to_owned()),
    ];
    info.title = html::find(body, "h1", |t| {
        t.attr("data-testid") == Some("non-editable-product-title")
    })
    .map(|(_, inner)| html_to_text(inner))
    .unwrap_or_default();
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn albums() {
        let page = "https://music.apple.com/jp/album/x/1503302894";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<h1 data-testid="non-editable-product-title"> Album </h1>
            <picture><source srcset="https://is1-ssl.mzstatic.com/image/thumb/Music/a.jpg/296x296bb.webp 296w, https://x 592w"></picture>"#;
        let info = page_info(&known, page, body).unwrap();
        assert_eq!(
            info.files,
            ["https://a1.mzstatic.com/us/r1000/0/Music/a.jpg"]
        );
        assert_eq!(info.title, "Album");
    }
}
