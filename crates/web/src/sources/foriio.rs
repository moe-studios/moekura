//! Foriio works, from the state the page preloads.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let state = html::json_after(&body, "window.__PRELOADED_STATE__")
        .ok_or("Foriio: no work in the page")?;
    let id = page.rsplit('/').next().unwrap_or_default();
    work_info(known, page, &state["works"][id]["data"]).ok_or_else(|| "Foriio: no such work".into())
}

fn work_info(known: &SourceUrl, page: &str, work: &Value) -> Option<SourceInfo> {
    let images = work["images"].as_array()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = images
        .iter()
        .filter_map(|i| i["urls"]["list"].as_str())
        .map(|url| {
            moekura_core::sites::parse(url)
                .and_then(|u| u.file_url)
                .unwrap_or_else(|| url.to_owned())
        })
        .collect();
    let author = &work["author"];
    info.artist_name = author["profile"]["name"].as_str().map(str::to_owned);
    if let Some(name) = author["screen_name"].as_str() {
        info.artist_account = Some(name.to_owned());
        info.profile_urls
            .push(format!("https://www.foriio.com/{name}"));
    }
    info.profile_urls.extend(
        author["profile"]["twitter_url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .map(str::to_owned),
    );
    info.title = text_of(&work["title"]);
    info.description = text_of(&work["description"]);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn works() {
        let page = "https://www.foriio.com/works/600743";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<script>window.__PRELOADED_STATE__ = {"works": {"600743": {"data": {
            "title": "Cat", "description": "",
            "images": [{"urls": {"list": "https://foriio.imgix.net/store/46d7.jpg?w=2184"}}],
            "author": {"screen_name": "comori22", "profile": {"name": "Comori"}}}}}};</script>"#;
        let state = html::json_after(body, "window.__PRELOADED_STATE__").unwrap();
        let info = work_info(&known, page, &state["works"]["600743"]["data"]).unwrap();
        assert_eq!(info.files, ["https://foriio.imgix.net/store/46d7.jpg"]);
        assert_eq!(info.profile_urls, ["https://www.foriio.com/comori22"]);
    }
}
