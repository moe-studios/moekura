//! Ko-fi gallery items, shop items and posts, from their pages.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, key};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let url = url::Url::parse(page).map_err(|e| e.to_string())?;
    let item = url
        .query_pairs()
        .find(|(k, _)| k == "viewimage")
        .map(|(_, v)| v.into_owned())
        .or_else(|| page.strip_prefix("https://ko-fi.com/i/").map(str::to_owned));
    if let Some(item) = item {
        let item = key(&item)?;
        let body = http
            .page(
                &format!("https://ko-fi.com/Gallery/LoadGalleryItem?galleryItemId={item}"),
                &[],
            )
            .await?;
        return gallery_item(known, page, &body).ok_or_else(|| "Ko-fi: no such item".into());
    }
    let body = http.page(page, &[]).await?;
    let info = if page.starts_with("https://ko-fi.com/s/") {
        shop_item(known, page, &body)
    } else {
        post(known, page, &body)
    };
    if info.files.is_empty() && info.title.is_empty() {
        return Err("Ko-fi: nothing in the page".into());
    }
    Ok(info)
}

fn gallery_item(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let mut info = SourceInfo::new(known.site, page);
    // The full-size download, else the large image.
    let downloads: Vec<String> = html::tags(body, "a")
        .into_iter()
        .filter(|a| a.has_class("label-hires"))
        .filter_map(|a| a.attr("href").map(str::to_owned))
        .collect();
    info.files = if downloads.is_empty() {
        html::tags(body, "img")
            .into_iter()
            .filter(|i| i.attr("id").is_some_and(|id| id.starts_with("hires")))
            .filter_map(|i| i.attr("src").map(str::to_owned))
            .collect()
    } else {
        downloads
    };
    if info.files.is_empty() {
        return None;
    }
    if let Some((_, thumb)) = html::find(body, "div", |t| t.has_class("gallery-item-thumb"))
        && let Some(href) = html::tags(thumb, "a")
            .first()
            .and_then(|a| a.attr("href").map(str::to_owned))
    {
        info.profile_urls.push(format!(
            "https://ko-fi.com/{}",
            href.trim_start_matches('/')
        ));
    }
    if let Some((_, caption)) = html::find(body, "div", |t| t.has_class("modal-caption-pdg")) {
        info.title = html::find(caption, "h2", |_| true)
            .map(|(_, inner)| html_to_text(inner))
            .unwrap_or_default();
        info.description = html::find(caption, "div", |_| true)
            .map(|(_, inner)| html_to_text(inner))
            .unwrap_or_default();
    }
    Some(info)
}

fn shop_item(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(body, "img")
        .into_iter()
        .filter(|i| i.has_class("kfds-c-carousel-product-img"))
        .filter_map(|i| i.attr("src").map(str::to_owned))
        .collect();
    if let Some(href) = html::tags(body, "a")
        .into_iter()
        .find(|a| a.has_class("navbar-creator"))
        .and_then(|a| a.attr("href").map(str::to_owned))
    {
        info.profile_urls.push(format!(
            "https://ko-fi.com/{}",
            href.trim_start_matches('/')
        ));
    }
    info.title = html::find(body, "div", |t| t.has_class("shop-item-title"))
        .or_else(|| html::find(body, "h1", |t| t.has_class("shop-item-title")))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::find(body, "div", |t| {
        t.has_class("kfds-c-product-detail-res-width")
    })
    .map(|(_, inner)| html_to_text(inner))
    .unwrap_or_default();
    info
}

fn post(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    if let Some((_, featured)) = html::find(body, "div", |t| t.has_class("article-featured-image"))
    {
        info.files.extend(
            html::tags(featured, "img")
                .into_iter()
                .filter_map(|i| i.attr("src").map(str::to_owned)),
        );
    }
    // The article's body is written into the page by a script.
    let article = html::between(body, "'<div class=\"fr-view article-body\">", "</div>';")
        .map(|a| a.replace("\\'", "'"))
        .unwrap_or_default();
    info.files.extend(
        html::tags(&article, "img")
            .into_iter()
            .filter_map(|i| i.attr("src").map(str::to_owned)),
    );
    info.title = html::find(body, "div", |t| t.has_class("article-title"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html_to_text(&article);
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_items_and_posts() {
        let page = "https://ko-fi.com/i/IU7U5SS0YZ";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div id="gallery-item-view"><a class="label-hires" href="https://storage.ko-fi.com/cdn/useruploads/display/a.png">Hi-res</a></div>
            <div class="gallery-item-thumb"><a href="/E1E7FH8ZY">x</a></div>
            <div class="modal-caption-pdg"><h2>Cat</h2><div>Drawn</div></div>"#;
        let info = gallery_item(&known, page, body).unwrap();
        assert_eq!(
            info.files,
            ["https://storage.ko-fi.com/cdn/useruploads/display/a.png"]
        );
        assert_eq!(info.profile_urls, ["https://ko-fi.com/E1E7FH8ZY"]);
        assert_eq!(
            (info.title.as_str(), info.description.as_str()),
            ("Cat", "Drawn")
        );
        let page = "https://ko-fi.com/post/Update-S6S0YPT5K";
        let body = r#"<div class="article-title"><h1>Update</h1></div>
            <script>shadowDom.innerHTML = '<div class="fr-view article-body"><p>It\'s done</p><img src="https://storage.ko-fi.com/b.png"></div>';</script>"#;
        let info = post(&known, page, body);
        assert_eq!(info.files, ["https://storage.ko-fi.com/b.png"]);
        assert_eq!(info.description, "It's done");
    }
}
