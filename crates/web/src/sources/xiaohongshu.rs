//! Xiaohongshu (RedNote) notes, from their page's initial state: the
//! images or video, the user, tags and text. Notes mostly need a login
//! (the `webId`, `web_session` and `gid` cookies).

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, key, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = key(page
        .split('?')
        .next()
        .unwrap_or(page)
        .rsplit('/')
        .next()
        .unwrap_or_default())?;
    // The token is the link's, where anything could be: it mustn't bring
    // more of a query with it.
    let url = url::Url::parse(page).map_err(|e| e.to_string())?;
    if url.fragment().is_some() || url.query_pairs().any(|(k, _)| k != "xsec_token") {
        return Err("Xiaohongshu: not a note".into());
    }
    let body = http.page(page, &[]).await?;
    let state = state_of(&body).ok_or("Xiaohongshu: no note in the page")?;
    note_info(known, page, &state["note"]["noteDetailMap"][id]["note"])
        .ok_or_else(|| "Xiaohongshu: no such note".into())
}

/// `window.__INITIAL_STATE__={…}`, which uses `undefined`.
fn state_of(body: &str) -> Option<Value> {
    let start = body.find("__INITIAL_STATE__")?;
    let from = start + body[start..].find('{')?;
    let end = from + body[from..].find("</script>")?;
    let json = body[from..end]
        .trim()
        .trim_end_matches(';')
        .replace("undefined", "null");
    serde_json::from_str(&json).ok()
}

fn note_info(known: &SourceUrl, page: &str, note: &Value) -> Option<SourceInfo> {
    note.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    if note["type"] == "video" {
        info.files.extend(
            note["video"]["consumer"]["originVideoKey"]
                .as_str()
                .map(|key| format!("https://sns-video-bd.xhscdn.com/{key}")),
        );
    } else {
        for image in note["imageList"].as_array().into_iter().flatten() {
            let streams = strings(&image["stream"]["h264"], Some("masterUrl"));
            if streams.is_empty() {
                info.files.extend(image["urlDefault"].as_str().map(|url| {
                    moekura_core::sites::parse(url)
                        .and_then(|u| u.file_url)
                        .unwrap_or_else(|| url.to_owned())
                }));
            } else {
                info.files.extend(streams);
            }
        }
    }
    let user = &note["user"];
    info.artist_name = user["nickname"].as_str().map(str::to_owned);
    if let Some(id) = user["userId"].as_str() {
        info.profile_urls
            .push(format!("https://www.xiaohongshu.com/user/profile/{id}"));
    }
    info.tags = tags_named(strings(&note["tagList"], Some("name")));
    info.title = text_of(&note["title"]);
    info.description = text_of(&note["desc"]);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes() {
        let page = "https://www.xiaohongshu.com/explore/6421b331000000002702901f";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<script>window.__INITIAL_STATE__={"note":{"noteDetailMap":{"6421b331000000002702901f":{"note":{
            "type":"normal","title":"Cat","desc":"Hi","tagList":[{"name":"cat"}],"video":undefined,
            "user":{"userId":"6234917d0000000010008cf8","nickname":"Neko"},
            "imageList":[{"urlDefault":"https://sns-webpic-qc.xhscdn.com/202405050857/60985d4963cfb500a9b0838667eb3adc/1000g00828idf6nofk05g5ohki5uk137o8beqcv8!nd_dft_wgth_webp_3"}]}}}}}</script>"#;
        let state = state_of(body).unwrap();
        let info = note_info(
            &known,
            page,
            &state["note"]["noteDetailMap"]["6421b331000000002702901f"]["note"],
        )
        .unwrap();
        assert_eq!(
            info.files,
            ["https://ci.xiaohongshu.com/1000g00828idf6nofk05g5ohki5uk137o8beqcv8"]
        );
        assert_eq!(
            info.profile_urls,
            ["https://www.xiaohongshu.com/user/profile/6234917d0000000010008cf8"]
        );
    }
}
