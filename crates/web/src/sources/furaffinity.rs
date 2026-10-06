//! Fur Affinity submissions, from their pages: the download link, tags,
//! artist and description. Mature submissions need a login (the `a` and
//! `b` cookies).

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    submission_info(known, page, &body)
        .ok_or_else(|| "Fur Affinity: no submission in the page".into())
}

fn submission_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let options = html::find(body, "section", |t| {
        t.attr("id") == Some("submission-options")
    })
    .or_else(|| html::find(body, "div", |t| t.attr("id") == Some("submission-options")))
    .map(|(_, inner)| inner)
    .unwrap_or(body);
    let download = html::tags(options, "a")
        .into_iter()
        .find(|a| html::label(options, a).as_deref() == Some("Download"))?;
    let href = download.attr("href")?;
    let file = if href.starts_with("//") {
        format!("https:{href}")
    } else if href.starts_with('/') {
        format!("https://d.furaffinity.net{href}")
    } else {
        href.to_owned()
    };
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![file];
    info.tags = tags_named(
        html::tags(body, "a")
            .into_iter()
            .filter(|a| a.has_class("tag") || a.attr("rel") == Some("tag"))
            .filter_map(|a| html::label(body, &a))
            .collect::<Vec<_>>(),
    );
    info.artist_name = html::find(body, "span", |t| {
        t.has_class("c-usernameBlockSimple__displayName")
    })
    .map(|(_, inner)| html_to_text(inner));
    if let Some(user) = html::tags(body, "a")
        .into_iter()
        .filter_map(|a| a.attr("href").map(str::to_owned))
        .find(|href| href.starts_with("/user/"))
    {
        let name = user.trim_matches('/').trim_start_matches("user/");
        info.artist_account = Some(name.to_owned());
        info.profile_urls
            .push(format!("https://www.furaffinity.net/user/{name}"));
    }
    info.title = html::find(body, "div", |t| t.has_class("submission-title"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::find(body, "div", |t| t.has_class("submission-description"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submissions() {
        let page = "https://www.furaffinity.net/view/46821705";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<a href="/user/iwbitu/"><img class="submission-user-icon"></a>
            <div class="submission-title"><h2><p>Yubi</p></h2></div>
            <span class="c-usernameBlockSimple__displayName">Iwbitu</span>
            <section id="submission-options"><div class="aligncenter"><a class="button" href="//d.furaffinity.net/art/iwbitu/1650222955/1650222955.iwbitu_yubi.jpg">Download</a></div></section>
            <div class="submission-description user-submitted-links">Thanks</div>
            <section class="tags-row"><span class="tags"><a href="/search/@keywords cat" rel="tag">cat</a></span></section>"#;
        let info = submission_info(&known, page, body).unwrap();
        assert_eq!(
            info.files,
            ["https://d.furaffinity.net/art/iwbitu/1650222955/1650222955.iwbitu_yubi.jpg"]
        );
        assert_eq!(
            info.profile_urls,
            ["https://www.furaffinity.net/user/iwbitu"]
        );
        assert_eq!(info.tags[0].name, "cat");
        assert_eq!(info.title, "Yubi");
    }
}
