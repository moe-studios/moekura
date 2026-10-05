//! piapro contents, from their pages: the illustration as shown (its
//! download needs a login), the creator, tags and text.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    content_info(known, page, &body).ok_or_else(|| "piapro: no content in the page".into())
}

fn absolute(href: &str) -> String {
    if href.starts_with('/') {
        format!("https://piapro.jp{href}")
    } else {
        href.to_owned()
    }
}

fn content_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let mut info = SourceInfo::new(known.site, page);
    // The content's own id, for short links.
    if let Some(id) = html::tags(body, "input")
        .into_iter()
        .find(|t| t.attr("id") == Some("DownloadContent_contentId"))
        .and_then(|t| t.attr("value").map(str::to_owned))
    {
        info.page_url = format!("https://piapro.jp/content/{id}");
    }
    if let Some((_, illust)) = html::find(body, "div", |t| t.has_class("contents_illust_img")) {
        info.files = html::tags(illust, "img")
            .into_iter()
            .filter_map(|t| t.attr("src").map(absolute))
            .collect();
    }
    if let Some((_, creator)) = html::find(body, "div", |t| t.has_class("contents_creator")) {
        if let Some(link) = html::tags(creator, "a").into_iter().next()
            && let Some(href) = link.attr("href")
        {
            info.profile_urls.push(absolute(href));
        }
        info.artist_name = html::find(creator, "p", |t| t.has_class("contents_creator_txt"))
            .or_else(|| html::find(creator, "span", |t| t.has_class("contents_creator_txt")))
            .map(|(_, inner)| html_to_text(inner));
    }
    info.title = html::find(body, "h1", |t| t.has_class("contents_title"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::find(body, "div", |t| t.has_class("contents_description"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.tags = tags_named(
        html::find(body, "div", |t| t.has_class("contents_taglist"))
            .or_else(|| html::find(body, "ul", |t| t.has_class("contents_taglist")))
            .map(|(_, list)| {
                html::tags(list, "a")
                    .into_iter()
                    .filter_map(|a| html::label(list, &a))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    (!info.files.is_empty() || !info.title.is_empty()).then_some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contents() {
        let page = "https://piapro.jp/t/_J0y";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<input id="DownloadContent_contentId" value="w22xmltnyzcsrqxu">
            <h1 class="contents_title">Miku</h1>
            <div class="contents_creator"><a href="/nibiirooo_"><p class="contents_creator_txt">Nibi</p></a></div>
            <div class="contents_illust_img"><img src="https://cdn.piapro.jp/thumb_i/w2/w22xmltnyzcsrqxu_20240303200945_0860_0600.png"></div>
            <div class="contents_taglist"><span class="tag"><a href="/t">初音ミク</a></span></div>"#;
        let info = content_info(&known, page, body).unwrap();
        assert_eq!(info.page_url, "https://piapro.jp/content/w22xmltnyzcsrqxu");
        assert_eq!(info.profile_urls, ["https://piapro.jp/nibiirooo_"]);
        assert_eq!(info.tags[0].name, "初音ミク");
    }
}
