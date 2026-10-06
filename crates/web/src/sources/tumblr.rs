//! Tumblr posts: from the API when there's a key for it
//! (`[sources.logins."api.tumblr.com"]`, `query = { api_key = "…" }`), else
//! from the post's page on tumblr.com (its content blocks).

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html, html_to_text, key, number, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    // https://<blog>.tumblr.com/post/<id>
    let blog = page
        .strip_prefix("https://")
        .and_then(|p| p.split(".tumblr.com").next())
        .ok_or("Tumblr: not a post")?;
    let blog = key(blog)?;
    let id = number(page.rsplit('/').next().unwrap_or_default())?;
    if http.has_login("api.tumblr.com") {
        let answer = http
            .json(
                &format!("https://api.tumblr.com/v2/blog/{blog}/posts?id={id}"),
                &[],
            )
            .await?;
        return api_info(known, page, &answer["response"]["posts"][0])
            .ok_or_else(|| "Tumblr: no such post".into());
    }
    let body = http
        .page(
            &format!("https://www.tumblr.com/{blog}/{id}"),
            &[("Accept", "text/html")],
        )
        .await?;
    let state =
        html::script_json(&body, "___INITIAL_STATE___").ok_or("Tumblr: no post in the page")?;
    let post = state["PeeprRoute"]["initialTimeline"]["objects"]
        .as_array()
        .and_then(|o| {
            o.iter()
                .find(|p| p["objectType"] == "post" || p["id"].is_string())
        })
        .cloned()
        .unwrap_or_default();
    blocks_info(known, page, &post).ok_or_else(|| "Tumblr: no such post".into())
}

/// The API's (legacy) post.
fn api_info(known: &SourceUrl, page: &str, post: &Value) -> Option<SourceInfo> {
    post.as_object()?;
    let mut info = SourceInfo::new(known.site, page);
    for photo in post["photos"].as_array().into_iter().flatten() {
        let biggest = std::iter::once(&photo["original_size"])
            .chain(photo["alt_sizes"].as_array().into_iter().flatten())
            .max_by_key(|s| s["width"].as_u64().unwrap_or(0) * s["height"].as_u64().unwrap_or(0));
        info.files
            .extend(biggest.and_then(|s| s["url"].as_str()).map(str::to_owned));
    }
    info.files
        .extend(post["video_url"].as_str().map(str::to_owned));
    let text = ["body", "description", "caption", "answer"]
        .iter()
        .find_map(|k| post[*k].as_str())
        .unwrap_or_default();
    info.files.extend(
        html::tags(text, "img")
            .into_iter()
            .filter_map(|t| t.attr("src").map(str::to_owned)),
    );
    info.files = info.files.into_iter().map(png).collect();
    blog_of(&mut info, post["blog_name"].as_str());
    info.tags = tags_named(strings(&post["tags"], None));
    info.title = text_of(&post["title"]);
    info.description = html_to_text(text);
    Some(info)
}

/// A post on tumblr.com: Tumblr's content blocks (NPF).
fn blocks_info(known: &SourceUrl, page: &str, post: &Value) -> Option<SourceInfo> {
    let content = post["content"].as_array()?;
    let mut info = SourceInfo::new(known.site, page);
    let mut text = Vec::new();
    for block in content {
        match block["type"].as_str() {
            // The first of an image's media is its largest.
            Some("image") => info
                .files
                .extend(block["media"][0]["url"].as_str().map(str::to_owned)),
            Some("video") => info
                .files
                .extend(block["media"]["url"].as_str().map(str::to_owned)),
            Some("text") => text.push(text_of(&block["text"])),
            _ => {}
        }
    }
    info.files = info.files.into_iter().map(png).collect();
    blog_of(
        &mut info,
        post["blogName"]
            .as_str()
            .or_else(|| post["blog"]["name"].as_str()),
    );
    info.tags = tags_named(strings(&post["tags"], None));
    info.description = text.join("\n");
    Some(info)
}

/// `.pnj` is a page showing a PNG: the PNG is the file.
fn png(url: String) -> String {
    match url.strip_suffix(".pnj") {
        Some(stem) => format!("{stem}.png"),
        None => url,
    }
}

fn blog_of(info: &mut SourceInfo, blog: Option<&str>) {
    if let Some(blog) = blog {
        info.artist_account = Some(blog.to_owned());
        info.profile_urls.push(format!("https://{blog}.tumblr.com"));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn posts() {
        let page = "https://marmaladica.tumblr.com/post/188237914346";
        let known = moekura_core::sites::parse(page).unwrap();
        let post = json!({
            "blogName": "marmaladica", "tags": ["art"],
            "content": [
                { "type": "image", "media": [{ "url": "https://64.media.tumblr.com/a/s2048x3072/b.png" }, { "url": "https://x/small.png" }] },
                { "type": "text", "text": "Saved" }
            ]
        });
        let info = blocks_info(&known, page, &post).unwrap();
        assert_eq!(
            info.files,
            ["https://64.media.tumblr.com/a/s2048x3072/b.png"]
        );
        assert_eq!(info.profile_urls, ["https://marmaladica.tumblr.com"]);
        assert_eq!(info.description, "Saved");
        let legacy = json!({
            "blog_name": "marmaladica", "type": "photo", "caption": "<p>Saved</p>",
            "photos": [{ "original_size": { "url": "https://x/1280.png", "width": 1280, "height": 1000 },
                         "alt_sizes": [{ "url": "https://x/500.png", "width": 500, "height": 400 }] }]
        });
        let info = api_info(&known, page, &legacy).unwrap();
        assert_eq!(info.files, ["https://x/1280.png"]);
    }
}
