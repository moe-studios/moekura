//! Minitokyo gallery entries, from their pages (whose HTML is too broken
//! to read as such, so the download link is found in the text).

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    entry_info(known, page, &body).ok_or_else(|| "Minitokyo: no image in the page".into())
}

fn entry_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let download =
        regex::Regex::new(r"(?i)https?://static\d*\.minitokyo\.net/downloads/\d+/\d+/\d+\.[a-z]+")
            .ok()?
            .find(body)?
            .as_str()
            .to_owned();
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![download];
    if let Some((_, menu)) = html::find(body, "div", |t| t.attr("id") == Some("menu"))
        && let Some((link, name)) = html::find(menu, "h2", |_| true).and_then(|(_, h2)| {
            html::find(h2, "a", |_| true).map(|(t, inner)| (t, html_to_text(inner)))
        })
    {
        info.artist_name = Some(name);
        info.profile_urls
            .extend(link.attr("href").map(str::to_ascii_lowercase));
    }
    info.tags = tags_named(
        html::find(body, "ul", |t| t.attr("id") == Some("tag-cloud"))
            .or_else(|| html::find(body, "div", |t| t.attr("id") == Some("tag-cloud")))
            .map(|(_, cloud)| {
                html::tags(cloud, "a")
                    .into_iter()
                    .map(|a| html_to_text(html::inner(cloud, &a)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    info.title = html::find(body, "h1", |_| true)
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries() {
        let page = "http://gallery.minitokyo.net/view/365677";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div id="menu"><h2><a href="http://Deto15.minitokyo.net">deto15</a></h2></div><h1>Cat</h1>
            <a href="http://static.minitokyo.net/downloads/31/33/764181.jpg">Download</a>
            <ul id="tag-cloud"><li><a href="/Touhou">Touhou</a></li></ul>"#;
        let info = entry_info(&known, page, body).unwrap();
        assert_eq!(
            info.files,
            ["http://static.minitokyo.net/downloads/31/33/764181.jpg"]
        );
        assert_eq!(info.profile_urls, ["http://deto15.minitokyo.net"]);
        assert_eq!(info.tags[0].name, "Touhou");
    }
}
