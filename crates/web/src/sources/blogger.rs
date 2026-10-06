//! Blogger posts and pages, from their HTML: the post's images (at their
//! originals), title, labels and text.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    post_info(known, page, &body).ok_or_else(|| "Blogger: no post in the page".into())
}

fn post_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let (_, post) = html::find(body, "div", |t| t.has_class("post-body"))?;
    let mut info = SourceInfo::new(known.site, page);
    // Images are usually links to their originals.
    let linked: Vec<String> = html::tags(post, "a")
        .into_iter()
        .filter_map(|a| a.attr("href").map(str::to_owned))
        .filter(|href| moekura_core::sites::parse(href).is_some_and(|u| u.is_file))
        .collect();
    let images: Vec<String> = html::tags(post, "img")
        .into_iter()
        .filter_map(|i| {
            i.attr("data-src")
                .or_else(|| i.attr("src"))
                .map(str::to_owned)
        })
        .collect();
    info.files = if linked.is_empty() { images } else { linked }
        .into_iter()
        .map(|url| {
            moekura_core::sites::parse(&url)
                .and_then(|u| u.file_url)
                .unwrap_or(url)
        })
        .collect();
    info.files.dedup();
    info.title = html::meta(body, "og:title").unwrap_or_default();
    info.tags = tags_named(
        html::tags(body, "a")
            .into_iter()
            .filter(|a| a.attr("rel") == Some("tag"))
            .filter_map(|a| html::label(body, &a))
            .collect::<Vec<_>>(),
    );
    info.description = html_to_text(post);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts() {
        let page = "https://benbotport.blogspot.com/2011/06/mass-effect-2.html";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<meta property="og:title" content="Mass Effect 2">
            <div class="post-body entry-content"><a href="https://4.bp.blogspot.com/-a/b/c/d/s1600/x.jpg"><img src="https://4.bp.blogspot.com/-a/b/c/d/s400/x.jpg"></a><br>Done!</div>
            <span class="post-labels"><a href="/search/label/fanart" rel="tag">fanart</a></span>"#;
        let info = post_info(&known, page, body).unwrap();
        assert_eq!(info.files, ["https://1.bp.blogspot.com/-a/b/c/d/d/x.jpg"]);
        assert_eq!(info.title, "Mass Effect 2");
        assert_eq!(info.tags[0].name, "fanart");
        assert_eq!(info.description, "Done!");
    }
}
