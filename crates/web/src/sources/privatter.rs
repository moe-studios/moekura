//! Privatter posts, from their pages.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let info = page_info(known, page, &body);
    if info.files.is_empty() && info.description.is_empty() {
        return Err("Privatter: no post in the page".into());
    }
    Ok(info)
}

fn page_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(body, "a")
        .into_iter()
        .filter_map(|a| a.attr("href").map(str::to_owned))
        .filter(|href| href.contains("d2pqhom6oey9wx.cloudfront.net/img_original/"))
        .collect();
    info.files.dedup();
    if let Some(user) = html::tags(body, "a")
        .into_iter()
        .filter_map(|a| a.attr("href").map(str::to_owned))
        .find(|href| href.starts_with("/u/"))
    {
        let name = user.trim_start_matches("/u/");
        info.artist_account = Some(name.to_owned());
        info.profile_urls
            .push(format!("https://privatter.net/u/{name}"));
    }
    info.title = html::find(body, "p", |t| t.has_class("lead"))
        .or_else(|| html::find(body, "h1", |t| t.has_class("lead")))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::find(body, "p", |t| t.has_class("honbun"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts() {
        let page = "https://privatter.net/i/7184521";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<a href="/u/GLK_Sier">GLK</a><p class="lead">Title</p><p class="honbun">Text</p>
            <p class="image"><a href="https://d2pqhom6oey9wx.cloudfront.net/img_original/65015.png"><img></a></p>"#;
        let info = page_info(&known, page, body);
        assert_eq!(
            info.files,
            ["https://d2pqhom6oey9wx.cloudfront.net/img_original/65015.png"]
        );
        assert_eq!(info.profile_urls, ["https://privatter.net/u/GLK_Sier"]);
    }
}
