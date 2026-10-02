//! Bilibili opus and dynamic posts, from the web API (which wants a
//! browser's user agent and a `buvid3` cookie): the pictures, the author,
//! topics and text. Videos and other pages are read for OpenGraph tags.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, id_of, opengraph, tags_named, text_of};

const AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:143.0) Gecko/20100101 Firefox/143.0";

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let Some(id) = page.strip_prefix("https://www.bilibili.com/opus/") else {
        // Videos, illustrations, articles: what their pages say.
        let url = url::Url::parse(page).map_err(|e| e.to_string())?;
        let mut info = opengraph::fetch(http, &url)
            .await?
            .unwrap_or_else(|| SourceInfo::new(known.site, page));
        info.site = known.site.name;
        return Ok(info);
    };
    let buvid = http
        .json(
            "https://api.bilibili.com/x/web-frontend/getbuvid",
            &[("User-Agent", AGENT)],
        )
        .await
        .ok()
        .and_then(|v| v["data"]["buvid"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let cookie = format!("buvid3={buvid}");
    let headers = [("User-Agent", AGENT), ("Cookie", cookie.as_str())];
    let opus = http
        .json(
            &format!("https://api.bilibili.com/x/polymer/web-dynamic/v1/opus/detail?id={id}&features=htmlNewStyle"),
            &headers,
        )
        .await?;
    opus_info(known, page, &opus["data"]["item"]).ok_or_else(|| "Bilibili: no such post".into())
}

/// The opus item's modules, merged into one object (the API lists them).
fn modules(item: &Value) -> serde_json::Map<String, Value> {
    let mut merged = serde_json::Map::new();
    match &item["modules"] {
        Value::Array(list) => {
            for module in list.iter().filter_map(Value::as_object) {
                // Every module lists every key, null but for its own.
                for (k, v) in module {
                    if k != "module_type" && !v.is_null() {
                        merged.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                }
            }
        }
        Value::Object(map) => merged = map.clone(),
        _ => {}
    }
    merged
}

fn original(url: &str) -> String {
    moekura_core::sites::parse(url)
        .and_then(|u| u.file_url)
        .unwrap_or_else(|| url.to_owned())
}

fn opus_info(known: &SourceUrl, page: &str, item: &Value) -> Option<SourceInfo> {
    let modules = modules(item);
    let author = modules.get("module_author")?;
    let mut info = SourceInfo::new(known.site, page);
    let top = &modules.get("module_top").cloned().unwrap_or_default()["display"]["album"]["pics"];
    info.files = top
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["url"].as_str())
        .map(original)
        .collect();
    let paragraphs = modules
        .get("module_content")
        .and_then(|c| c["paragraphs"].as_array().cloned())
        .unwrap_or_default();
    let mut text = Vec::new();
    let mut topics = Vec::new();
    for paragraph in &paragraphs {
        match paragraph["para_type"].as_u64() {
            Some(2) => info.files.extend(
                paragraph["pic"]["pics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["url"].as_str())
                    .map(original),
            ),
            _ => {
                let nodes = paragraph["text"]["nodes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut line = String::new();
                for node in &nodes {
                    line.push_str(node["word"]["words"].as_str().unwrap_or_default());
                    let rich = &node["rich"];
                    line.push_str(rich["text"].as_str().unwrap_or_default());
                    if rich["type"] == "RICH_TEXT_NODE_TYPE_TOPIC" {
                        topics.push(text_of(&rich["text"]).trim_matches('#').to_owned());
                    }
                }
                if !line.trim().is_empty() {
                    text.push(line);
                }
            }
        }
    }
    info.artist_name = author["name"].as_str().map(str::to_owned);
    if let Some(mid) = id_of(&author["mid"]) {
        info.artist_account = Some(format!("bilibili_{mid}"));
        info.profile_urls
            .push(format!("https://space.bilibili.com/{mid}"));
    }
    if let Some(topic) = modules.get("module_topic").and_then(|t| t["name"].as_str()) {
        topics.push(topic.to_owned());
    }
    info.tags = tags_named(topics);
    info.title = modules
        .get("module_title")
        .map(|t| text_of(&t["text"]))
        .unwrap_or_default();
    info.description = text.join("\n");
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn opus_posts() {
        let page = "https://www.bilibili.com/opus/684571925561737250";
        let known = moekura_core::sites::parse(page).unwrap();
        let item = json!({ "modules": [
            { "module_type": "MODULE_TYPE_AUTHOR", "module_author": { "name": "Neko", "mid": 355143 } },
            { "module_type": "MODULE_TYPE_TOP", "module_top": { "display": { "album": { "pics": [
                { "url": "https://i0.hdslb.com/bfs/new_dyn/37f77871d417c76a08a9467527e9670810c4c442.jpg@1036w.webp" }
            ] } } } },
            { "module_type": "MODULE_TYPE_CONTENT", "module_content": { "paragraphs": [
                { "para_type": 1, "text": { "nodes": [
                    { "type": "TEXT_NODE_TYPE_WORD", "word": { "words": "New art " } },
                    { "type": "TEXT_NODE_TYPE_RICH", "rich": { "type": "RICH_TEXT_NODE_TYPE_TOPIC", "text": "#原神#" } }
                ] } }
            ] } }
        ]});
        let info = opus_info(&known, page, &item).unwrap();
        assert_eq!(
            info.files,
            ["https://i0.hdslb.com/bfs/new_dyn/37f77871d417c76a08a9467527e9670810c4c442.jpg"]
        );
        assert_eq!(info.profile_urls, ["https://space.bilibili.com/355143"]);
        assert_eq!(info.tags[0].name, "原神");
        assert_eq!(info.description, "New art #原神#");
    }
}
