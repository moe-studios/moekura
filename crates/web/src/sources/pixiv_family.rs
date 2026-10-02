//! Pixiv's other sites: Pixiv Sketch items (its JSON API), Pixiv Comic
//! (its app API, which wants a hash of the time salted with a value from
//! the page) and Pixiv Factory collections.

use moekura_core::sites::SourceUrl;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{Http, SourceInfo, SourceTag, html, html_to_text, id_of, text_of};

fn tags_from(values: &[&Value]) -> Vec<SourceTag> {
    values
        .iter()
        .filter_map(|t| t.as_str())
        .map(|t| SourceTag {
            name: t.to_owned(),
            translation: None,
        })
        .collect()
}

pub(super) async fn sketch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = page.rsplit('/').next().unwrap_or_default();
    let item = http
        .json(
            &format!("https://sketch.pixiv.net/api/items/{id}.json"),
            &[],
        )
        .await?;
    sketch_item(known, page, &item).ok_or_else(|| "Pixiv Sketch: no such item".into())
}

fn sketch_item(known: &SourceUrl, page: &str, item: &Value) -> Option<SourceInfo> {
    let data = &item["data"];
    let user = &data["user"];
    let name = user["unique_name"].as_str()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = data["media"]
        .as_array()
        .map(|media| {
            media
                .iter()
                .filter_map(|m| m["photo"]["original"]["url2x"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    info.headers = vec![("Referer", "https://sketch.pixiv.net/".to_owned())];
    info.artist_name = user["name"].as_str().map(str::to_owned);
    info.artist_account = Some(name.to_owned());
    info.profile_urls = vec![format!("https://sketch.pixiv.net/@{name}")];
    if let Some(id) = id_of(&user["pixiv_user_id"]) {
        info.profile_urls
            .push(format!("https://www.pixiv.net/users/{id}"));
    }
    let tags: Vec<&Value> = data["tags"]
        .as_array()
        .map(|t| t.iter().collect())
        .unwrap_or_default();
    info.tags = tags_from(&tags);
    info.description = text_of(&data["text"]);
    Some(info)
}

const COMIC_REFERER: &str = "https://comic.pixiv.net/";

/// Pixiv Comic's app API: the page's salt hashed with the time.
async fn comic_api(http: &Http<'_>, salt: &str, url: &str) -> Result<Value, String> {
    let time = jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string();
    let hash = hex::encode(Sha256::digest(format!("{time}{salt}")));
    http.json(
        url,
        &[
            ("X-Requested-With", "pixivcomic"),
            ("X-Client-Hash", &hash),
            ("X-Client-Time", &time),
            ("Referer", COMIC_REFERER),
        ],
    )
    .await
}

/// A story's pages, or a work's or magazine's cover, from Pixiv Comic.
pub(super) async fn comic(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    let salt = html::script_json(&body, "__NEXT_DATA__")
        .and_then(|d| d["props"]["pageProps"]["salt"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let path = page.trim_start_matches("https://comic.pixiv.net/");
    let id = path.rsplit('/').next().unwrap_or_default();
    let api = "https://comic.pixiv.net/api/app";
    let (url, pick): (String, fn(&Value) -> &Value) = if path.starts_with("viewer/stories/") {
        (
            format!("{api}/episodes/{id}/read_v4"),
            |v| &v["data"]["reading_episode"],
        )
    } else if path.starts_with("novel/viewer/stories/") {
        (format!("{api}/novel/episodes/{id}/read_v4"), |v| {
            &v["data"]["reading_episode"]
        })
    } else if path.starts_with("works/") {
        (format!("{api}/works/v5/{id}"), |v| &v["data"])
    } else if path.starts_with("novel/works/") {
        (format!("{api}/novel/works/{id}"), |v| &v["data"])
    } else {
        (format!("{api}/magazines/v2/{id}"), |v| &v["data"])
    };
    let answer = comic_api(http, &salt, &url).await?;
    comic_info(known, page, pick(&answer)).ok_or_else(|| "Pixiv Comic: nothing there".into())
}

fn comic_info(known: &SourceUrl, page: &str, data: &Value) -> Option<SourceInfo> {
    if !data.is_object() {
        return None;
    }
    let mut info = SourceInfo::new(known.site, page);
    info.headers = vec![("Referer", COMIC_REFERER.to_owned())];
    // A story's pages, else a cover.
    let original = |url: &str| {
        moekura_core::sites::parse(url)
            .and_then(|u| u.file_url)
            .unwrap_or_else(|| url.to_owned())
    };
    if let Some(pages) = data["pages"].as_array() {
        info.files = pages
            .iter()
            .filter_map(|p| p["url"].as_str())
            .map(original)
            .collect();
        info.title = text_of(&data["title"]);
        return Some(info);
    }
    let work = &data["official_work"];
    let magazine = &data["magazine"];
    let cover = work["image"]["main_big"]
        .as_str()
        .or_else(|| magazine["image"]["main"].as_str());
    info.files = cover.map(original).into_iter().collect();
    let author = work["author"].as_str().map(|a| {
        // Novels name an author and an illustrator: the artist is the latter.
        a.split_once("イラスト：")
            .map_or(a, |(_, illustrator)| {
                illustrator.lines().next().unwrap_or_default()
            })
            .trim()
            .to_owned()
    });
    info.artist_name = author;
    let mut tags: Vec<&Value> = Vec::new();
    for list in [&work["categories"], &work["tags"]] {
        tags.extend(list.as_array().into_iter().flatten().map(|t| &t["name"]));
    }
    info.tags = tags_from(&tags);
    info.title = text_of(&work["name"]).trim().to_owned();
    if info.title.is_empty() {
        info.title = text_of(&magazine["name"]);
    }
    let description = work["description"]
        .as_str()
        .or_else(|| magazine["description"].as_str())
        .unwrap_or_default();
    info.description = html_to_text(description);
    Some(info)
}

/// A Pixiv Factory collection's images (or the one the link's
/// `#image-<id>` names), and its title and description from the page.
pub(super) async fn factory(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let (collection, image) = match page.split_once("#image-") {
        Some((collection, image)) => (collection, image.parse::<u64>().ok()),
        None => (page, None),
    };
    let name = collection.rsplit('/').next().unwrap_or_default();
    let images = http
        .json(
            &format!("https://factory.pixiv.net/api/v1/palette/collections/{name}/images"),
            &[],
        )
        .await?;
    let body = http.page(collection, &[]).await.unwrap_or_default();
    factory_info(known, page, &images, image, &body)
        .ok_or_else(|| "Pixiv Factory: no such collection".into())
}

fn factory_info(
    known: &SourceUrl,
    page: &str,
    images: &Value,
    image: Option<u64>,
    body: &str,
) -> Option<SourceInfo> {
    let list = images["images"].as_array()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = list
        .iter()
        .filter(|i| image.is_none() || i["id"].as_u64() == image)
        .filter_map(|i| i["url"]["canvas"].as_str())
        .map(|path| format!("https://factory.pixiv.net{path}"))
        .collect();
    info.title = html::find(body, "h1", |_| true)
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::meta(body, "description").unwrap_or_default();
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
    fn sketch_items() {
        let page = "https://sketch.pixiv.net/items/5835314698645024323";
        let item = json!({ "data": {
            "text": "Hello #cat", "tags": ["cat"],
            "user": { "unique_name": "neko", "name": "Neko", "pixiv_user_id": "5" },
            "media": [{ "photo": { "original": { "url2x": "https://img-sketch.pixiv.net/uploads/a.png" } } }]
        }});
        let info = sketch_item(&known(page), page, &item).unwrap();
        assert_eq!(info.files, ["https://img-sketch.pixiv.net/uploads/a.png"]);
        assert_eq!(
            info.profile_urls,
            [
                "https://sketch.pixiv.net/@neko",
                "https://www.pixiv.net/users/5"
            ]
        );
        assert_eq!(info.tags[0].name, "cat");
    }

    #[test]
    fn comic_stories_and_works() {
        let page = "https://comic.pixiv.net/viewer/stories/162153";
        let story = json!({ "title": "Ch. 1", "pages": [
            { "url": "https://img-comic.pximg.net/c/q90_gridshuffle32:32/images/page/162153/abc/1.jpg?20240112151247" }
        ]});
        let info = comic_info(&known(page), page, &story).unwrap();
        assert_eq!(
            info.files,
            ["https://img-comic.pximg.net/images/page/162153/abc/1.jpg"]
        );
        let page = "https://comic.pixiv.net/novel/works/3877";
        let work = json!({ "official_work": {
            "name": "Book", "author": "著者：A\r\nイラスト：B", "description": "<p>Hi</p>",
            "image": { "main_big": "https://public-img-comic.pximg.net/images/work_main/3877.jpg" },
            "tags": [{ "name": "fantasy" }]
        }});
        let info = comic_info(&known(page), page, &work).unwrap();
        assert_eq!(info.artist_name.as_deref(), Some("B"));
        assert_eq!(info.description, "Hi");
        assert_eq!(info.tags[0].name, "fantasy");
    }

    #[test]
    fn factory_collections() {
        let page = "https://factory.pixiv.net/palette/collections/imys_tachie#image-2";
        let images = json!({ "images": [
            { "id": 1, "url": { "canvas": "/resources/images/1/canvas" } },
            { "id": 2, "url": { "canvas": "/resources/images/2/canvas" } }
        ]});
        let info = factory_info(
            &known(page),
            page,
            &images,
            Some(2),
            "<main><h1>Tachie</h1></main>",
        )
        .unwrap();
        assert_eq!(
            info.files,
            ["https://factory.pixiv.net/resources/images/2/canvas"]
        );
        assert_eq!(info.title, "Tachie");
    }
}
