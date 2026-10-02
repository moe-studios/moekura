//! Posts on other boorus, read through the same APIs as bulk imports
//! ([`moekura_core::remote`]): Danbooru, e621, the Gelbooru family and
//! Moebooru. Rule34.us and Zerochan have their own. A post's own source,
//! when it's a page another strategy reads, gives the artist, their
//! profiles and the commentary, as on Danbooru.

use moekura_core::remote::{self, Kind, PostRef, RemotePost};
use moekura_core::sites::SourceUrl;
use serde_json::Value;
use url::Url;

use super::{Http, SourceInfo, SourceTag, html, text_of};

/// The booru kind and API base of a site.
fn kind_of(key: &str) -> Option<(Kind, &'static str)> {
    Some(match key {
        "danbooru" => (Kind::Danbooru, "https://danbooru.donmai.us"),
        "e621" => (Kind::E621, "https://e621.net"),
        "gelbooru" => (Kind::Gelbooru, "https://gelbooru.com"),
        "safebooru" => (Kind::Gelbooru, "https://safebooru.org"),
        "tbib" => (Kind::Gelbooru, "https://tbib.org"),
        "rule34_xxx" => (Kind::Gelbooru, "https://api.rule34.xxx"),
        "yandere" => (Kind::Moebooru, "https://yande.re"),
        "konachan" => (Kind::Moebooru, "https://konachan.com"),
        _ => return None,
    })
}

/// The post a canonical page URL names: `…/posts/<id>`, `…&id=<id>`,
/// `…/post/show/<id>`, or a `md5`.
fn post_ref(page: &Url) -> Option<(Option<i64>, Option<String>)> {
    let md5 = page
        .query_pairs()
        .find(|(k, _)| k == "md5")
        .map(|(_, v)| v.into_owned());
    let id = page
        .query_pairs()
        .find(|(k, _)| k == "id")
        .and_then(|(_, v)| v.parse().ok())
        .or_else(|| page.path_segments()?.next_back()?.parse().ok());
    (md5.is_some() || id.is_some()).then_some((id, md5))
}

/// A post's tags without their category prefixes, and its artists'.
fn tags_of(post: &RemotePost) -> (Vec<SourceTag>, Vec<String>) {
    let mut tags = Vec::new();
    let mut artists = Vec::new();
    for tag in &post.tags {
        let (category, name) = tag.split_once(':').unwrap_or(("", tag));
        if category == "artist" {
            artists.push(name.to_owned());
        }
        tags.push(SourceTag {
            name: name.to_owned(),
            translation: None,
        });
    }
    (tags, artists)
}

/// The post as a [`SourceInfo`].
pub(super) fn info(known: &SourceUrl, post: &RemotePost) -> SourceInfo {
    let (tags, artists) = tags_of(post);
    let mut info = SourceInfo::new(known.site, post.page_url.clone());
    info.files = post.file_url.iter().cloned().collect();
    info.artist_account = artists.first().cloned();
    info.artist_name = artists.first().cloned();
    info.tags = tags;
    info
}

/// e621 leaves logged-out visitors without the file's URL; it follows
/// from the MD5.
fn e621_file(answer: &Value) -> Option<String> {
    let file = &answer["post"]["file"];
    let md5 = file["md5"].as_str()?;
    let ext = file["ext"].as_str()?;
    Some(format!(
        "https://static1.e621.net/data/{}/{}/{md5}.{ext}",
        md5.get(..2)?,
        md5.get(2..4)?
    ))
}

/// Moebooru leaves deleted posts without the file's URL, but the file is
/// usually still where its MD5 says.
fn moebooru_file(site: &str, answer: &Value) -> Option<String> {
    let post = &answer[0];
    let md5 = post["md5"].as_str()?;
    let ext = post["file_ext"].as_str().unwrap_or("png");
    let host = if site.contains("yande.re") {
        "https://files.yande.re"
    } else {
        site
    };
    Some(format!("{host}/image/{md5}.{ext}"))
}

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
    depth: u8,
) -> Result<SourceInfo, String> {
    let (kind, base) = kind_of(known.site.key).ok_or("not a booru")?;
    let page = Url::parse(page).map_err(|e| e.to_string())?;
    let (id, md5) = post_ref(&page).ok_or("no post in the link")?;
    let which = match (id, md5.as_deref()) {
        (Some(id), _) => PostRef::Id(id),
        (None, Some(md5)) => PostRef::Md5(md5),
        _ => return Err("no post in the link".into()),
    };
    let url = remote::post_url(kind, base, which);
    // The canonical page's site, not the API's host, names the post.
    let site = format!("https://{}", page.host_str().unwrap_or_default());
    let (_, body) = http.text(&url, &[("Accept", "application/json")]).await?;
    let post = remote::parse_post(kind, &site, &body)?
        .ok_or_else(|| format!("{}: no such post", known.site.name))?;
    let mut info = info(known, &post);
    if info.files.is_empty() {
        let answer: Value = serde_json::from_str(&body).unwrap_or_default();
        info.files.extend(match kind {
            Kind::E621 => e621_file(&answer),
            Kind::Moebooru => moebooru_file(&site, &answer),
            _ => None,
        });
    }
    if depth == 0 {
        add_original(http, &mut info, &post.source).await;
    }
    Ok(info)
}

/// What the post's own source says about its artist and their words, if
/// another strategy reads it.
pub(super) async fn add_original(http: &Http<'_>, info: &mut SourceInfo, source: &str) {
    let Ok(url) = Url::parse(source) else {
        return;
    };
    if !matches!(url.scheme(), "http" | "https") || source == info.page_url {
        return;
    }
    let Ok(Some(original)) = http.nested(&url).await else {
        return;
    };
    if original.artist_name.is_some() || original.artist_account.is_some() {
        info.artist_name = original.artist_name;
        info.artist_account = original.artist_account;
    }
    info.profile_urls = original.profile_urls;
    info.title = original.title;
    info.description = original.description;
    info.tags.extend(original.tags);
}

/// Rule34.us posts: its page names the file and lists the tags.
pub(super) async fn rule34_us(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    rule34_us_page(known, page, &body).ok_or_else(|| "Rule34.us: no such post".into())
}

fn rule34_us_page(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let file = html::tags(body, "a")
        .into_iter()
        .filter_map(|a| a.attr("href").map(str::to_owned))
        .find(|href| href.contains("/images/"))?;
    // The title ends with the post's id, for links by MD5.
    let id = html::title(body).and_then(|t| {
        t.rsplit(|c: char| !c.is_ascii_digit())
            .next()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
    });
    let page_url = match id {
        Some(id) if page.contains("hotlink.php") => {
            format!("https://rule34.us/index.php?r=posts/view&id={id}")
        }
        _ => page.to_owned(),
    };
    let mut info = SourceInfo::new(known.site, page_url);
    info.files = vec![file];
    info.tags = html::meta(body, "keywords")
        .map(|k| {
            k.split(", ")
                .filter(|t| !t.is_empty())
                .map(|t| SourceTag {
                    name: t.replace(' ', "_"),
                    translation: None,
                })
                .collect()
        })
        .unwrap_or_default();
    Some(info)
}

/// Zerochan entries, from its JSON (`<page>?json`).
pub(super) async fn zerochan(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = page
        .trim_end_matches("#full")
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let entry = http
        .json(&format!("https://www.zerochan.net/{id}?json"), &[])
        .await?;
    zerochan_entry(known, page, &entry).ok_or_else(|| "Zerochan: no such entry".into())
}

fn zerochan_entry(known: &SourceUrl, page: &str, entry: &Value) -> Option<SourceInfo> {
    let full = entry["full"].as_str()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = vec![full.to_owned()];
    info.tags = entry["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|t| t.as_str())
                .map(|t| SourceTag {
                    name: t.to_owned(),
                    translation: None,
                })
                .collect()
        })
        .unwrap_or_default();
    info.title = text_of(&entry["title"]);
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn known(url: &str) -> SourceUrl {
        moekura_core::sites::parse(url).unwrap()
    }

    #[test]
    fn posts_from_the_api() {
        let page =
            Url::parse("https://gelbooru.com/index.php?page=post&s=view&id=7798045").unwrap();
        assert_eq!(post_ref(&page), Some((Some(7_798_045), None)));
        let page = Url::parse("https://danbooru.donmai.us/posts?md5=abc").unwrap();
        assert_eq!(post_ref(&page), Some((None, Some("abc".into()))));
        let site = known("https://danbooru.donmai.us/posts/1");
        let post = remote::parse_post(
            Kind::Danbooru,
            "https://danbooru.donmai.us",
            r#"{"id": 1, "rating": "g", "file_url": "https://cdn.donmai.us/original/a.png",
                "tag_string_artist": "neko", "tag_string_general": "cat solo", "source": ""}"#,
        )
        .unwrap()
        .unwrap();
        let info = info(&site, &post);
        assert_eq!(info.site, "Danbooru");
        assert_eq!(info.page_url, "https://danbooru.donmai.us/posts/1");
        assert_eq!(info.files, ["https://cdn.donmai.us/original/a.png"]);
        assert_eq!(info.artist_account.as_deref(), Some("neko"));
        let names: Vec<&str> = info.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["neko", "cat", "solo"]);
        assert_eq!(
            moebooru_file(
                "https://konachan.com",
                &json!([{ "md5": "e2e2", "file_ext": "png" }])
            ),
            Some("https://konachan.com/image/e2e2.png".into())
        );
        assert_eq!(
            e621_file(
                &json!({ "post": { "file": { "md5": "6d1a6090ea82c2524212499797e7e53a", "ext": "png" } } })
            ),
            Some("https://static1.e621.net/data/6d/1a/6d1a6090ea82c2524212499797e7e53a.png".into())
        );
    }

    #[test]
    fn rule34_us_and_zerochan() {
        let site = known("https://rule34.us/hotlink.php?hash=236690fd962fa394edf9894450261dac");
        let body = r#"<title>Rule34 - If it exists / sora / 6204967</title>
            <meta name="keywords" content="sora, kingdom hearts">
            <div class="tag-list-left"><a href="https://img2.rule34.us/images/23/66/236690fd962fa394edf9894450261dac.png">Original</a></div>"#;
        let info = rule34_us_page(
            &site,
            "https://rule34.us/hotlink.php?hash=236690fd962fa394edf9894450261dac",
            body,
        )
        .unwrap();
        assert_eq!(
            info.page_url,
            "https://rule34.us/index.php?r=posts/view&id=6204967"
        );
        assert_eq!(info.tags[1].name, "kingdom_hearts");
        let site = known("https://www.zerochan.net/90674");
        let entry = json!({ "full": "https://static.zerochan.net/a.full.90674.jpg", "tags": ["Cat"], "title": "" });
        let info = zerochan_entry(&site, "https://www.zerochan.net/90674#full", &entry).unwrap();
        assert_eq!(info.files, ["https://static.zerochan.net/a.full.90674.jpg"]);
        assert_eq!(info.tags[0].name, "Cat");
    }
}
