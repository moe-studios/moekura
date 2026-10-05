//! Naver Blog posts (from the mobile page's properties) and Naver Cafe
//! articles (from the cafe article API).

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, key, number, strings, tags_named, text_of};

pub(super) async fn blog(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    // The blog's name may come from the link's query, where anything
    // could be.
    let (name, post) = page
        .strip_prefix("https://blog.naver.com/")
        .and_then(|path| path.split_once('/'))
        .ok_or("Naver Blog: not a post")?;
    key(name)?;
    number(post)?;
    let mobile = page.replacen("://blog.naver.com/", "://m.blog.naver.com/", 1);
    let body = http.page(&mobile, &[]).await?;
    blog_info(known, page, &body).ok_or_else(|| "Naver Blog: no post in the page".into())
}

/// A path with its Korean file name in EUC-KR, as blogfiles.naver.net
/// wants it.
fn euc_kr_path(path: &str) -> String {
    let decoded = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    let (bytes, _, _) = encoding_rs::EUC_KR.encode(&decoded);
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn blog_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    // The post's details are attributes of a few hidden elements.
    let mut properties: Vec<(String, String)> = Vec::new();
    for id in [
        "_post_property",
        "_photo_view_property",
        "_floating_menu_property",
    ] {
        for tag in ["div", "span", "input"] {
            if let Some(found) = html::tags(body, tag)
                .into_iter()
                .find(|t| t.attr("id") == Some(id))
            {
                properties.extend(found.attrs);
            }
        }
    }
    let property = |name: &str| {
        properties
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let images: Value = serde_json::from_str(&property("attachimagepathandidinfo")?).ok()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = strings(&images, Some("path"))
        .iter()
        .map(|path| format!("http://blogfiles.naver.net{}", euc_kr_path(path)))
        .collect();
    info.artist_name = html::meta(body, "naverblog:nickname");
    info.title = property("title").unwrap_or_default();
    let tags = html::between(body, "var gsTagName = \"", "\";").unwrap_or_default();
    info.tags = tags_named(tags.split(','));
    info.description = html::find(body, "div", |t| t.attr("id") == Some("viewTypeSelector"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    Some(info)
}

pub(super) async fn cafe(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    // https://cafe.naver.com/ca-fe/cafes/<club>/articles/<id>, or
    // https://cafe.naver.com/<club name>/<id>, whose club id the page says.
    let parts: Vec<&str> = page.split('/').collect();
    let (club, article) = match parts.as_slice() {
        [.., "cafes", club, "articles", id] => ((*club).to_owned(), *id),
        [.., _, id] => {
            let body = http.page(page, &[]).await?;
            let club = html::between(&body, "clubid=", "&")
                .or_else(|| html::between(&body, "g_sClubId = \"", "\""))
                .ok_or("Naver Cafe: no club in the page")?
                .to_owned();
            (club, *id)
        }
        _ => return Err("Naver Cafe: not an article".into()),
    };
    let (club, article) = (number(&club)?.to_owned(), number(article)?);
    let answer = http
        .json(
            &format!(
                "https://apis.naver.com/cafe-web/cafe-articleapi/v4/cafes/{club}/articles/{article}"
            ),
            &[("X-Cafe-Product", "pc")],
        )
        .await?;
    cafe_info(known, page, &club, &answer["result"])
        .ok_or_else(|| "Naver Cafe: no such article".into())
}

fn cafe_info(known: &SourceUrl, page: &str, club: &str, result: &Value) -> Option<SourceInfo> {
    let article = &result["article"];
    article.as_object()?;
    let mut content = text_of(&article["contentHtml"]);
    // Attached images take their place in the text.
    for (i, element) in article["contentElements"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let html = match element["json"]["image"]["url"].as_str() {
            Some(url) if element["type"] == "IMAGE" => format!("<img src=\"{url}\">"),
            _ => String::new(),
        };
        content = content.replace(&format!("[[[CONTENT-ELEMENT-{i}]]]"), &html);
    }
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(&content, "img")
        .into_iter()
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .filter_map(|src| {
            moekura_core::sites::parse(&src)
                .filter(|u| u.site.key == "naver_cafe")?
                .file_url
        })
        .collect();
    let writer = &article["writer"];
    info.artist_name = writer["nick"].as_str().map(str::to_owned);
    if let Some(member) = writer["memberKey"].as_str() {
        info.profile_urls.push(format!(
            "https://cafe.naver.com/ca-fe/cafes/{club}/members/{member}"
        ));
    }
    if let Some(name) = result["cafe"]["url"].as_str() {
        info.profile_urls
            .push(format!("https://cafe.naver.com/{name}"));
    }
    info.tags = tags_named(strings(&result["tags"], None));
    info.title = text_of(&article["subject"]);
    info.description = html_to_text(&content);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn blog_posts() {
        let page = "https://blog.naver.com/kkid9624/223421884109";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<meta property="naverblog:nickname" content="Kid">
            <div id="_post_property" title="Art" attachimagepathandidinfo='[{"path": "/MjAy/MDAx.PNG/%EC%A0%9C%EB%B3%B8.PNG"}]'></div>
            <script>var gsTagName = "cat,dog";</script><div id="viewTypeSelector"><p>Hi</p></div>"#;
        let info = blog_info(&known, page, body).unwrap();
        assert_eq!(
            info.files,
            ["http://blogfiles.naver.net/MjAy/MDAx.PNG/%C1%A6%BA%BB.PNG"]
        );
        assert_eq!(info.tags.len(), 2);
        assert_eq!(info.title, "Art");
    }

    #[test]
    fn cafe_articles() {
        let page = "https://cafe.naver.com/ca-fe/cafes/29767250/articles/785";
        let known = moekura_core::sites::parse(page).unwrap();
        let result = json!({
            "cafe": { "url": "masterofeternity" }, "tags": ["art"],
            "article": {
                "subject": "Art", "writer": { "nick": "Neko", "memberKey": "Iep9" },
                "contentHtml": "<p>Hi</p>[[[CONTENT-ELEMENT-0]]]",
                "contentElements": [{ "type": "IMAGE", "json": { "image": { "url": "https://cafeptthumb-phinf.pstatic.net/a/b.png?type=w800" } } }]
            }
        });
        let info = cafe_info(&known, page, "29767250", &result).unwrap();
        assert_eq!(info.files, ["http://cafefiles.naver.net/a/b.png"]);
        assert_eq!(
            info.profile_urls[1],
            "https://cafe.naver.com/masterofeternity"
        );
    }
}
