//! Yachiyo's Room oekaki, from their pages.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    oekaki_info(known, page, &body).ok_or_else(|| "Yachiyo's Room: no oekaki in the page".into())
}

fn oekaki_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let file = html::tags(body, "img")
        .into_iter()
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .find(|src| src.contains("/prod/oekaki/"))?;
    let file = if file.starts_with('/') {
        format!("https://yachiyo-room.com{file}")
    } else {
        file
    };
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![file];
    let links = html::tags(body, "a");
    if let Some(artist) = links.iter().find(|a| {
        a.attr("href")
            .is_some_and(|h| h.starts_with("/gallery?name="))
    }) {
        let name = html_to_text(html::inner(body, artist));
        let encoded: String = url::form_urlencoded::byte_serialize(name.as_bytes()).collect();
        info.profile_urls
            .push(format!("https://yachiyo-room.com/gallery?name={encoded}"));
        info.artist_name = Some(name);
    }
    info.tags = tags_named(
        links
            .iter()
            .filter(|a| {
                a.attr("href")
                    .is_some_and(|h| h.starts_with("/gallery?tag="))
            })
            .map(|a| html_to_text(html::inner(body, a)))
            .collect::<Vec<_>>(),
    );
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oekaki() {
        let page = "https://yachiyo-room.com/oekaki/1059";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<img src="/img/prod/oekaki/1774796101015-w8euiu.png">
            <a href="/gallery?name=abc">abc</a><a href="/gallery?tag=cat">cat</a>"#;
        let info = oekaki_info(&known, page, body).unwrap();
        assert_eq!(
            info.profile_urls,
            ["https://yachiyo-room.com/gallery?name=abc"]
        );
        assert_eq!(info.tags[0].name, "cat");
    }
}
