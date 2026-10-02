//! Poipiku posts: the details from the page, the images from the form
//! the page loads them with (which wants a login, the `POIPIKU_LK` cookie,
//! for most posts; passworded posts are tried with "y" and "yes").

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let mut info = page_info(known, page, &body);
    // https://poipiku.com/<user>/<post>.html, after redirects.
    let canonical = info.page_url.clone();
    let parts: Vec<&str> = canonical
        .trim_end_matches(".html")
        .rsplit('/')
        .take(2)
        .collect();
    let [post, user] = parts.as_slice() else {
        return Err("Poipiku: not a post".into());
    };
    for password in ["", "y", "yes"] {
        let answer = http
            .post_form(
                "https://poipiku.com/f/ShowIllustDetailF.jsp",
                &[
                    ("Referer", "https://poipiku.com"),
                    ("Cookie", "POIPIKU_CONTENTS_VIEW_MODE=1"),
                ],
                &[("ID", user), ("TD", post), ("PAS", password)],
            )
            .await?;
        let answer: Value = serde_json::from_str(&answer).unwrap_or_default();
        if answer["result"] == 1 {
            info.files = images(answer["html"].as_str().unwrap_or_default());
            break;
        }
    }
    if info.files.is_empty() {
        return Err("Poipiku: the images need a login".into());
    }
    Ok(info)
}

fn images(fragment: &str) -> Vec<String> {
    html::tags(fragment, "img")
        .into_iter()
        .filter(|t| t.has_class("DetailIllustItemImage"))
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
        .collect()
}

fn page_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let canonical = html::tags(body, "link")
        .into_iter()
        .find(|t| t.attr("rel") == Some("canonical"))
        .and_then(|t| t.attr("href").map(str::to_owned));
    let mut info = SourceInfo::new(known.site, canonical.unwrap_or_else(|| page.to_owned()));
    info.artist_name = html::find(body, "h2", |t| t.has_class("UserInfoUserName"))
        .or_else(|| html::find(body, "div", |t| t.has_class("UserInfoUserName")))
        .map(|(_, inner)| html_to_text(inner));
    info.description = html::find(body, "div", |t| t.has_class("IllustItemDesc"))
        .or_else(|| html::find(body, "h1", |t| t.has_class("IllustItemDesc")))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.tags = tags_named(
        html::tags(body, "div")
            .into_iter()
            .filter(|t| t.has_class("TagName"))
            .map(|t| {
                html_to_text(html::inner(body, &t))
                    .trim_start_matches('#')
                    .to_owned()
            })
            .collect::<Vec<_>>(),
    );
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts() {
        let page = "https://poipiku.com/6849873/8271386.html";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<link rel="canonical" href="https://poipiku.com/6849873/8271386.html">
            <h2 class="UserInfoUserName"><a>Neko</a></h2><h1 class="IllustItemDesc">Cats</h1>
            <div class="IllustItemTag"><div class="TagName">#cat</div></div>"#;
        let info = page_info(&known, page, body);
        assert_eq!(info.artist_name.as_deref(), Some("Neko"));
        assert_eq!(info.tags[0].name, "cat");
        let fragment = r#"<img class="DetailIllustItemImage" src="https://img.poipiku.com/user_img02/006849873/008271386_016865825_S968sAh7Y.jpeg_640.jpg">"#;
        assert_eq!(images(fragment).len(), 1);
    }
}
