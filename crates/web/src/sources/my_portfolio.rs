//! Adobe Portfolio pages: the project's images and text.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    Ok(page_info(known, page, &body))
}

fn page_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    let modules = html::find(body, "div", |t| t.attr("id") == Some("project-modules"))
        .map(|(_, inner)| inner)
        .unwrap_or_default();
    info.files = html::tags(modules, "div")
        .into_iter()
        .chain(html::tags(modules, "img"))
        .filter_map(|t| t.attr("data-src").map(str::to_owned))
        .collect();
    info.files.dedup();
    // <title>Artist - Site description - Work</title>
    let title = html::title(body).unwrap_or_default();
    let mut parts = title.split(" - ");
    info.artist_name = parts
        .next()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    info.title = parts.collect::<Vec<_>>().join(" - ").trim().to_owned();
    let description = html::find(body, "p", |t| t.has_class("description"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    let text = html_to_text(modules);
    info.description = [description, text]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects() {
        let page = "https://sekigahara023.myportfolio.com/eaapexlegends5";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<title>TOOCO - Visual Art - Paradise Bird</title>
            <div id="project-modules"><div class="module"><img data-src="https://cdn.myportfolio.com/a/b.png"></div><p>Made in 2024</p></div>"#;
        let info = page_info(&known, page, body);
        assert_eq!(info.files, ["https://cdn.myportfolio.com/a/b.png"]);
        assert_eq!(info.artist_name.as_deref(), Some("TOOCO"));
        assert_eq!(info.title, "Visual Art - Paradise Bird");
        assert_eq!(info.description, "Made in 2024");
    }
}
