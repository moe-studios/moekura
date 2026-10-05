//! Fantia posts, from its API (which wants the page's CSRF token, so
//! reading posts needs a login, the `_session_id` cookie), and products,
//! from their pages.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, id_of, number, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = number(page.rsplit('/').next().unwrap_or_default())?;
    let body = http.page(page, &[]).await?;
    if page.contains("/products/") {
        return Ok(product_info(known, page, &body));
    }
    let token = html::meta(&body, "csrf-token").ok_or("Fantia: no token in the page")?;
    let api = http
        .json(
            &format!("https://fantia.jp/api/v1/posts/{id}"),
            &[
                ("X-CSRF-Token", &token),
                ("X-Requested-With", "XMLHttpRequest"),
            ],
        )
        .await?;
    post_info(known, page, &api["post"]).ok_or_else(|| "Fantia: no such post".into())
}

fn post_info(known: &SourceUrl, page: &str, post: &Value) -> Option<SourceInfo> {
    post.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files
        .extend(post["thumb_micro"].as_str().map(str::to_owned));
    for content in post["post_contents"].as_array().into_iter().flatten() {
        if content["visible_status"] != "visible" {
            continue;
        }
        match content["category"].as_str() {
            Some("photo_gallery") => info.files.extend(
                content["post_content_photos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["url"]["original"].as_str().map(str::to_owned)),
            ),
            Some("file") => info.files.extend(
                content["download_uri"]
                    .as_str()
                    .map(|uri| format!("https://fantia.jp{uri}")),
            ),
            Some("blog") => {
                let ops: Value = content["comment"]
                    .as_str()
                    .and_then(|c| serde_json::from_str(c).ok())
                    .unwrap_or_default();
                for op in ops["ops"].as_array().into_iter().flatten() {
                    let insert = &op["insert"];
                    if let Some(path) = insert["fantiaImage"]["original_url"].as_str() {
                        info.files.push(format!("https://fantia.jp{path}"));
                    } else if let Some(image) = insert["image"].as_str() {
                        info.files.push(image.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    let club = &post["fanclub"];
    info.artist_name = club["creator_name"].as_str().map(str::to_owned);
    if let Some(id) = id_of(&club["id"]) {
        info.profile_urls
            .push(format!("https://fantia.jp/fanclubs/{id}"));
    }
    info.tags = tags_named(strings(&post["tags"], Some("name")));
    info.title = text_of(&post["title"]);
    info.description = text_of(&post["comment"]);
    Some(info)
}

fn product_info(known: &SourceUrl, page: &str, body: &str) -> SourceInfo {
    let mut info = SourceInfo::new(known.site, page);
    info.files = html::tags(body, "img")
        .into_iter()
        .filter(|t| t.has_class("img-fluid"))
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .filter(|src| src.contains("/uploads/") && !src.contains("/fallback/"))
        .collect();
    if let Some((tag, inner)) = html::find(body, "h1", |t| t.has_class("fanclub-name"))
        .or_else(|| html::find(body, "div", |t| t.has_class("fanclub-name")))
        && let Some(link) = html::tags(inner, "a").into_iter().next()
    {
        let _ = tag;
        if let Some(href) = link.attr("href") {
            info.profile_urls.push(format!("https://fantia.jp{href}"));
        }
        // "Club (Artist)"
        let name = html_to_text(html::inner(inner, &link));
        info.artist_name = name
            .rsplit_once(" (")
            .map(|(_, artist)| artist.trim_end_matches(')').to_owned())
            .or(Some(name));
    }
    info.title = html::find(body, "h1", |t| t.has_class("product-title"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html::find(body, "div", |t| t.has_class("product-description"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts_and_products() {
        let page = "https://fantia.jp/posts/1148334";
        let known = moekura_core::sites::parse(page).unwrap();
        let post = json!({
            "title": "New", "comment": "Hi", "thumb_micro": "https://c.fantia.jp/uploads/post/file/1148334/micro_a.jpg",
            "fanclub": { "id": 64496, "creator_name": "Neko" }, "tags": [{ "name": "cat" }],
            "post_contents": [
                { "visible_status": "visible", "category": "photo_gallery", "post_content_photos": [{ "url": { "original": "https://c.fantia.jp/a.png" } }] },
                { "visible_status": "visible", "category": "file", "download_uri": "/posts/1148334/download/1" },
                { "visible_status": "hidden", "category": "photo_gallery", "post_content_photos": [{ "url": { "original": "https://c.fantia.jp/b.png" } }] }
            ]
        });
        let info = post_info(&known, page, &post).unwrap();
        assert_eq!(
            info.files,
            [
                "https://c.fantia.jp/uploads/post/file/1148334/micro_a.jpg",
                "https://c.fantia.jp/a.png",
                "https://fantia.jp/posts/1148334/download/1"
            ]
        );
        assert_eq!(info.profile_urls, ["https://fantia.jp/fanclubs/64496"]);
        let page = "https://fantia.jp/products/249638";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<h1 class="fanclub-name"><a href="/fanclubs/1">Club (Neko)</a></h1>
            <h1 class="product-title">Pack</h1>
            <div class="product-gallery-item"><img class="img-fluid" src="https://c.fantia.jp/uploads/product/image/249638/a.png"></div>"#;
        let info = product_info(&known, page, body);
        assert_eq!(
            info.files,
            ["https://c.fantia.jp/uploads/product/image/249638/a.png"]
        );
        assert_eq!(info.artist_name.as_deref(), Some("Neko"));
        assert_eq!(info.profile_urls, ["https://fantia.jp/fanclubs/1"]);
    }
}
