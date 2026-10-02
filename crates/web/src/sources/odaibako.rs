//! Odaibako answers, from their pages: the answer's images, who answered,
//! and the request and answer.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let info = page_info(known, page, &body);
    if info.files.is_empty() {
        return Err("Odaibako: no answer in the page".into());
    }
    Ok(info)
}

fn page_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    let links: Vec<String> = html::tags(body, "a")
        .into_iter()
        .filter_map(|a| a.attr("href").map(str::to_owned))
        .collect();
    info.files = links
        .iter()
        .filter(|href| href.contains("/post_images/"))
        .map(|href| {
            moekura_core::sites::parse(href)
                .and_then(|u| u.file_url)
                .unwrap_or_else(|| href.clone())
        })
        .collect();
    info.files.dedup();
    if let Some(user) = links.iter().find(|h| h.starts_with("/u/")) {
        let name = user.trim_start_matches("/u/");
        info.artist_account = Some(name.to_owned());
        info.profile_urls
            .push(format!("https://odaibako.net/u/{name}"));
    }
    info.description = html::meta(body, "og:description").unwrap_or_default();
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers() {
        let page = "https://odaibako.net/posts/01923bc559bc0fd9ac983610d654ea2d";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<meta property="og:description" content="Draw a cat">
            <a href="https://ccs.odaibako.net/w=1600/post_images/aaaaaariko/c126.jpeg.webp"><img></a>
            <a href="/u/aaaaaariko">Riko</a>"#;
        let info = page_info(&known, page, body);
        assert_eq!(
            info.files,
            ["https://ccs.odaibako.net/_/post_images/aaaaaariko/c126.jpeg"]
        );
        assert_eq!(info.profile_urls, ["https://odaibako.net/u/aaaaaariko"]);
    }
}
