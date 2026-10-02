//! TINAMI works, from their pages. Originals come from a form the page
//! posts (which, like most works, wants a login: the `Tinami2SESSID`
//! cookie); without one, the images as shown.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let mut info = work_info(known, page, &body).ok_or("TINAMI: no work in the page")?;
    let token = html::tags(&body, "input")
        .into_iter()
        .find(|t| t.attr("name") == Some("ethna_csrf"))
        .and_then(|t| t.attr("value").map(str::to_owned));
    let id = page.rsplit('/').next().unwrap_or_default();
    let sub_ids: Vec<String> = html::tags(&body, "td")
        .into_iter()
        .filter(|t| t.has_class("thumbnail_list"))
        .filter_map(|t| t.attr("sub_id").map(str::to_owned))
        .collect();
    if let Some(token) = token {
        let subs: Vec<Option<&str>> = if sub_ids.is_empty() {
            vec![None]
        } else {
            sub_ids.iter().map(|s| Some(s.as_str())).collect()
        };
        let mut originals = Vec::new();
        for sub in subs {
            let mut form = vec![
                ("action_view_original", "true"),
                ("cont_id", id),
                ("ethna_csrf", token.as_str()),
            ];
            form.extend(sub.map(|s| ("sub_id", s)));
            let answer = http
                .post_form(page, &[("Referer", page)], &form)
                .await
                .unwrap_or_default();
            originals.extend(
                html::tags(&answer, "img")
                    .into_iter()
                    .filter_map(|t| t.attr("src").map(str::to_owned))
                    .find(|src| src.starts_with("//img.tinami.com"))
                    .map(|src| format!("https:{src}")),
            );
        }
        if !originals.is_empty() {
            info.files = originals;
        }
    }
    Ok(info)
}

fn work_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let (_, view) = html::find(body, "div", |t| t.attr("id") == Some("view"))?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::find(view, "div", |t| t.has_class("viewbody"))
        .map(|(_, viewbody)| {
            html::tags(viewbody, "img")
                .into_iter()
                .filter_map(|t| t.attr("src").map(str::to_owned))
                .filter(|src| src.contains("img.tinami.com"))
                .map(|src| {
                    if src.starts_with("//") {
                        format!("https:{src}")
                    } else {
                        src
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    // <div class="prof"><a><img></a><p><a href="/creator/profile/1"><strong>Name</strong></a></p>
    if let Some((_, prof)) = html::find(view, "div", |t| t.has_class("prof"))
        && let Some((_, line)) = html::find(prof, "p", |_| true)
        && let Some((link, name)) = html::find(line, "a", |_| true)
    {
        info.artist_name = Some(html_to_text(name));
        if let Some(id) = link.attr("href").and_then(|h| h.rsplit('/').next()) {
            info.artist_account = Some(format!("tinami_{id}"));
            info.profile_urls
                .push(format!("https://www.tinami.com/creator/profile/{id}"));
        }
    }
    info.title = html::find(view, "h1", |_| true)
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::find(view, "div", |t| t.has_class("description"))
        .or_else(|| html::find(view, "p", |t| t.has_class("description")))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.tags = tags_named(
        html::tags(view, "a")
            .into_iter()
            .filter(|a| {
                a.attr("href")
                    .is_some_and(|h| h.starts_with("/search/list"))
            })
            .map(|a| html_to_text(html::inner(view, &a)))
            .collect::<Vec<_>>(),
    );
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn works() {
        let page = "https://www.tinami.com/view/461459";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div id="view"><div class="viewdata"><h1><span>Cat</span></h1></div>
            <div class="prof"><a href="/creator/profile/1624"><img class="prof_img"></a><p><a href="/creator/profile/1624"><strong>Neko</strong></a>さん</p></div>
            <div class="viewbody"><img class="captify" src="//img.tinami.com/illust2/img/419/5013fde3406b9.jpg"></div>
            <div class="tag"><a href="/search/list?keyword=cat">cat</a></div></div>"#;
        let info = work_info(&known, page, body).unwrap();
        assert_eq!(
            info.files,
            ["https://img.tinami.com/illust2/img/419/5013fde3406b9.jpg"]
        );
        assert_eq!(
            info.profile_urls,
            ["https://www.tinami.com/creator/profile/1624"]
        );
        assert_eq!(info.tags[0].name, "cat");
    }
}
