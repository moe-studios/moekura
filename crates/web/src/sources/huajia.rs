//! Huajia works, goods and commissions, from its app API.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html_to_text, id_of, strings, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = page.rsplit('/').next().unwrap_or_default();
    let api = "https://huajia.163.com/napp";
    let (url, key) = if page.contains("/main/works/") {
        (format!("{api}/work/detail?work_id={id}"), None)
    } else if page.contains("/main/goods/") {
        (
            format!("{api}/store/goods/detail?goods_id={id}"),
            Some("goods"),
        )
    } else if page.contains("/main/projects/") {
        (
            format!("{api}/commission/commission/detail?commission_id={id}"),
            Some("commission"),
        )
    } else {
        return Err("Huajia: not a work".into());
    };
    let answer = http.json(&url, &[]).await?;
    let data = match key {
        Some(key) => &answer["data"][key],
        None => &answer["data"],
    };
    work_info(known, page, key, data).ok_or_else(|| "Huajia: no such work".into())
}

fn original(url: &str) -> String {
    moekura_core::sites::parse(url)
        .and_then(|u| u.file_url)
        .unwrap_or_else(|| url.to_owned())
}

fn work_info(known: &SourceUrl, page: &str, key: Option<&str>, data: &Value) -> Option<SourceInfo> {
    data.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    let user = match key {
        None => {
            info.files = data["work"]["file_url"]
                .as_str()
                .map(original)
                .into_iter()
                .collect();
            &data["author"]
        }
        Some("goods") => {
            info.files = strings(&data["description_images"], Some("file_url"))
                .iter()
                .map(|u| original(u))
                .collect();
            info.title = text_of(&data["name"]);
            info.description = html_to_text(&text_of(&data["description"]));
            &data["user"]
        }
        Some(_) => {
            info.files = strings(&data["images"], Some("file_url"))
                .iter()
                .map(|u| original(u))
                .collect();
            info.title = text_of(&data["title"]);
            info.description = html_to_text(&text_of(&data["description"]));
            &Value::Null
        }
    };
    info.artist_name = user["name"].as_str().map(str::to_owned);
    if let Some(id) = id_of(&user["uid"]) {
        info.profile_urls
            .push(format!("https://huajia.163.com/main/profile/{id}"));
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn works() {
        let page = "https://huajia.163.com/main/works/8z4GdKoE";
        let known = moekura_core::sites::parse(page).unwrap();
        let data = json!({
            "work": { "file_url": "https://huajia.fp.ps.netease.com/file/664ae65bd56ea97215dc3e25JM5jBQGB05?fop=imageView/2/w/300" },
            "author": { "uid": "MBmloOn8", "name": "Neko" }
        });
        let info = work_info(&known, page, None, &data).unwrap();
        assert_eq!(
            info.files,
            ["https://huajia.fp.ps.netease.com/file/664ae65bd56ea97215dc3e25JM5jBQGB05"]
        );
        assert_eq!(
            info.profile_urls,
            ["https://huajia.163.com/main/profile/MBmloOn8"]
        );
    }
}
