//! Plurk plurks and responses, from the plurk's page: the images, the
//! poster, hashtags and text.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let url = url::Url::parse(page).map_err(|e| e.to_string())?;
    let id = url.path().rsplit('/').next().unwrap_or_default();
    let response = url
        .query_pairs()
        .find(|(k, _)| k == "r")
        .map(|(_, v)| v.into_owned());
    let body = http
        .page(&format!("https://www.plurk.com/s/p/{id}"), &[])
        .await?;
    plurk_info(known, page, &body, response.as_deref()).ok_or_else(|| "Plurk: no such plurk".into())
}

fn plurk_info(
    known: &SourceUrl,
    page: &str,
    body: &str,
    response: Option<&str>,
) -> Option<SourceInfo> {
    let (tag, plurk) = match response {
        Some(id) => html::find(body, "div", |t| t.attr("data-rid") == Some(id))?,
        None => html::find(body, "div", |t| t.has_class("bigplurk"))?,
    };
    let content = html::find(plurk, "div", |t| t.has_class("plurk_content"))
        .map(|(_, inner)| inner)
        .unwrap_or(plurk);
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(content, "a")
        .into_iter()
        .filter(|a| a.has_class("pictureservices"))
        .filter_map(|a| {
            let (_, inner) = html::find(content, "a", |t| t.end == a.end)?;
            html::tags(inner, "img")
                .first()?
                .attr("alt")
                .map(str::to_owned)
        })
        .collect();
    if let Some(name) = tag.attr("data-nick") {
        info.artist_account = Some(name.to_owned());
        info.profile_urls
            .push(format!("https://www.plurk.com/{name}"));
    }
    info.artist_name = html::find(plurk, "a", |t| t.has_class("nick"))
        .or_else(|| html::find(plurk, "span", |t| t.has_class("nick")))
        .map(|(_, inner)| html_to_text(inner));
    info.tags = tags_named(
        html::tags(content, "span")
            .into_iter()
            .filter(|s| s.has_class("hashtag"))
            .map(|s| {
                html_to_text(html::inner(content, &s))
                    .trim_start_matches('#')
                    .to_owned()
            })
            .collect::<Vec<_>>(),
    );
    info.description = html_to_text(content);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plurks() {
        let page = "https://www.plurk.com/p/om6zv4";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div class="bigplurk" data-nick="redeyehare"><a class="nick">Hare</a>
            <div class="plurk_content">New <span class="hashtag">#art</span>
            <a class="pictureservices" href="https://images.plurk.com/5wj6WD0r6y4rLN0DL3sqag.jpg"><img src="https://images.plurk.com/mx_5wj6WD0r6y4rLN0DL3sqag.jpg" alt="https://images.plurk.com/5wj6WD0r6y4rLN0DL3sqag.jpg"></a></div></div>"#;
        let info = plurk_info(&known, page, body, None).unwrap();
        assert_eq!(
            info.files,
            ["https://images.plurk.com/5wj6WD0r6y4rLN0DL3sqag.jpg"]
        );
        assert_eq!(info.profile_urls, ["https://www.plurk.com/redeyehare"]);
        assert_eq!(info.tags[0].name, "art");
    }
}
