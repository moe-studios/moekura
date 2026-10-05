//! Nijie illustrations and doujin, from their pages. Nijie shows nothing
//! without a login: its `NIJIEIJIEID` and `nijie_tok` cookies in
//! `[sources.logins."nijie.info"]`.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

const HEADERS: [(&str, &str); 1] = [("Cookie", "R18=1")];

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    if !http.has_login("nijie.info") {
        return Err("Nijie: needs a login".into());
    }
    let body = http.page(page, &HEADERS).await?;
    let popup = http
        .page(&page.replacen("view.php", "view_popup.php", 1), &HEADERS)
        .await
        .unwrap_or_default();
    work_info(known, page, &body, &popup)
        .ok_or_else(|| "Nijie: no such work (or the login expired)".into())
}

fn work_info(known: &SourceUrl, page: &str, body: &str, popup: &str) -> Option<SourceInfo> {
    let doujin = html::find(body, "div", |t| t.attr("id") == Some("dojin_left")).is_some();
    let mut info = SourceInfo::new(known.site, page);
    let window = html::find(popup, "div", |t| t.attr("id") == Some("img_window"))
        .map(|(_, inner)| inner)
        .unwrap_or_default();
    info.files = html::tags(window, "img")
        .into_iter()
        .filter(|t| t.has_class("box-shadow999"))
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .map(|src| {
            let src = if src.starts_with("//") {
                format!("https:{src}")
            } else {
                src
            };
            moekura_core::sites::parse(&src)
                .and_then(|u| u.file_url)
                .unwrap_or(src)
        })
        .collect();
    let artist = html::tags(body, "a").into_iter().find(|a| {
        a.has_class("name")
            || (doujin
                && a.attr("href")
                    .is_some_and(|h| h.contains("members.php?id=")))
    });
    if let Some(link) = artist {
        info.artist_name = Some(html_to_text(html::inner(body, &link)));
        if let Some(id) = link.attr("href").and_then(|h| h.split("id=").nth(1)) {
            info.artist_account = Some(format!("nijie_{id}"));
            info.profile_urls
                .push(format!("https://nijie.info/members.php?id={id}"));
        }
    }
    info.title = html::find(body, "h2", |t| t.has_class("illust_title"))
        .or_else(|| html::find(body, "p", |t| t.has_class("title")))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::find(body, "div", |t| t.attr("id") == Some("illust_text"))
        .or_else(|| html::find(body, "div", |t| t.attr("id") == Some("dojin_text")))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.tags = tags_named(
        html::find(body, "div", |t| t.attr("id") == Some("view-tag"))
            .map(|(_, tags)| {
                html::tags(tags, "a")
                    .into_iter()
                    .filter(|a| a.attr("href").is_some_and(|h| h.contains("search")))
                    .filter_map(|a| html::label(tags, &a))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn works() {
        let page = "https://nijie.info/view.php?id=218856";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<a class="name" href="members.php?id=236014">Neko</a><h2 class="illust_title">Cat</h2>
            <div id="illust_text"><p>Hi</p></div><div id="view-tag"><a href="search.php?word=cat">cat</a></div>"#;
        let popup = r#"<div id="img_window"><img class="box-shadow999" src="//pic.nijie.net/03/__rs_l120x120/nijie/17/14/236014/illust/218856_0_a_b.png"></div>"#;
        let info = work_info(&known, page, body, popup).unwrap();
        assert_eq!(
            info.files,
            ["https://pic.nijie.net/03/nijie/17/14/236014/illust/218856_0_a_b.png"]
        );
        assert_eq!(
            info.profile_urls,
            ["https://nijie.info/members.php?id=236014"]
        );
        assert_eq!(info.tags[0].name, "cat");
    }
}
