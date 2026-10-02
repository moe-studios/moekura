//! Galleria illustrations, from their pages.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[("Cookie", "SFL=3")]).await?;
    let info = page_info(known, page, &body);
    if info.files.is_empty() {
        return Err("Galleria: no illustration in the page".into());
    }
    Ok(info)
}

fn page_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(body, "img")
        .into_iter()
        .filter(|t| t.has_class("TimeLineIllustImageFile"))
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .map(|src| {
            if src.starts_with("//") {
                format!("https:{src}")
            } else {
                src
            }
        })
        .collect();
    let text = |class: &str| {
        ["div", "span", "h2", "a"].iter().find_map(|tag| {
            html::find(body, tag, |t| t.has_class(class)).map(|(_, inner)| html_to_text(inner))
        })
    };
    info.artist_name = text("TimeLineUserName");
    info.title = text("TimeLineIllustTitle").unwrap_or_default();
    info.description = text("TimeLineIllustDesc").unwrap_or_default();
    info.tags = tags_named(
        html::tags(body, "a")
            .into_iter()
            .filter(|a| a.has_class("AutoLinkTag") || a.has_class("AutoLinkMyTag"))
            .map(|a| {
                html_to_text(html::inner(body, &a))
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
    fn illustrations() {
        let page = "https://galleria.emotionflow.com/40775/660870.html";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div class="TimeLineUserName"><span>Neko</span></div>
            <h2 class="TimeLineIllustTitle">Cat</h2>
            <img class="TimeLineIllustImageFile" src="//galleria-img.emotionflow.com/user_img9/40775/i660870_869.jpeg">
            <div class="TimeLineIllustDesc">Hi <a class="AutoLinkTag">#cat</a></div>"#;
        let info = page_info(&known, page, body);
        assert_eq!(
            info.files,
            ["https://galleria-img.emotionflow.com/user_img9/40775/i660870_869.jpeg"]
        );
        assert_eq!(info.artist_name.as_deref(), Some("Neko"));
        assert_eq!(info.tags[0].name, "cat");
    }
}
