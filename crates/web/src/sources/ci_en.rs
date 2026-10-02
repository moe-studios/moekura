//! Ci-En articles, from their pages: the article's images and videos
//! (those for supporters need a login), the creator, tags and text.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http
        .page(page, &[("Cookie", "accepted_rating=r18g")])
        .await?;
    article_info(known, page, &body).ok_or_else(|| "Ci-En: no article in the page".into())
}

fn article_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let (_, article) = html::find(body, "article", |_| true)?;
    let mut info = SourceInfo::new(known.site, page);
    // The cover, unless it's a sample of an attachment.
    if let Some(cover) = html::meta(body, "og:image").filter(|c| !c.contains("/attachment/")) {
        info.files.push(cover);
    }
    for tag in html::tags(article, "vue-l-image") {
        info.files.extend(tag.attr("data-raw").map(str::to_owned));
    }
    for tag in html::tags(article, "vue-file-player") {
        if let Some(base) = tag.attr("base-path") {
            let key = tag.attr("auth-key").unwrap_or_default();
            info.files.push(format!(
                "{}/video-web.mp4?{key}",
                base.trim_end_matches('/')
            ));
        }
    }
    info.files.dedup();
    info.artist_name = html::find(body, "span", |t| t.has_class("e-userName"))
        .or_else(|| html::find(body, "p", |t| t.has_class("e-userName")))
        .map(|(_, inner)| html_to_text(inner));
    info.title = html::find(article, "h1", |t| t.has_class("article-title"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.tags = tags_named(
        html::find(body, "ul", |t| t.has_class("c-hashTagList"))
            .map(|(_, list)| {
                html::tags(list, "a")
                    .into_iter()
                    .map(|a| {
                        html_to_text(html::inner(list, &a))
                            .trim_start_matches('#')
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    info.description = html::find(article, "div", |t| t.has_class("article-body"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn articles() {
        let page = "https://ci-en.net/creator/11019/article/921762";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<meta property="og:image" content="https://media.ci-en.jp/public/article_cover/creator/00011019/a/image-1280-c.jpg">
            <span class="e-userName">Neko</span>
            <article><h1 class="article-title">New</h1><div class="article-body"><p>Hello</p>
            <vue-l-image data-raw="https://media.ci-en.jp/private/attachment/creator/00011019/b/upload/x.jpg?px-time=1"></vue-l-image></div></article>
            <ul class="c-hashTagList"><li><a href="/tag">#cat</a></li></ul>"#;
        let info = article_info(&known, page, body).unwrap();
        assert_eq!(info.files.len(), 2);
        assert_eq!(info.title, "New");
        assert_eq!(info.artist_name.as_deref(), Some("Neko"));
        assert_eq!(info.tags[0].name, "cat");
        assert_eq!(info.description, "Hello");
    }
}
