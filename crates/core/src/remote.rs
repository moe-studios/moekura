//! Importing posts from other boorus through their public APIs: what each
//! kind of site's API looks like, and turning its answers into
//! [`RemotePost`]s. No IO here; the web crate fetches.

use serde_json::Value;

use crate::posts::Rating;

/// Posts asked for per page.
pub const PAGE_SIZE: u32 = 100;

/// The kinds of site we can import from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Danbooru, and anything speaking its API (other Moekura sites).
    Danbooru,
    /// e621 and e926.
    E621,
    /// Gelbooru 0.2 (also Safebooru, Rule34 and others on it).
    Gelbooru,
    /// Moebooru (Konachan, yande.re).
    Moebooru,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Danbooru, Kind::E621, Kind::Gelbooru, Kind::Moebooru];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Danbooru => "danbooru",
            Kind::E621 => "e621",
            Kind::Gelbooru => "gelbooru",
            Kind::Moebooru => "moebooru",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Guesses the kind of a well-known site from its host.
    pub fn guess(host: &str) -> Option<Self> {
        let host = host.trim_start_matches("www.");
        match host {
            "danbooru.donmai.us" | "safebooru.donmai.us" | "testbooru.donmai.us" => {
                Some(Kind::Danbooru)
            }
            "e621.net" | "e926.net" => Some(Kind::E621),
            "gelbooru.com" | "safebooru.org" | "rule34.xxx" => Some(Kind::Gelbooru),
            "konachan.com" | "konachan.net" | "yande.re" => Some(Kind::Moebooru),
            _ => None,
        }
    }

    /// Whether pages go by id (`page=b<id>`), which stays right as new
    /// posts arrive, rather than by number.
    pub fn pages_by_id(self) -> bool {
        matches!(self, Kind::Danbooru | Kind::E621)
    }

    /// Whether the site's notes and pools can be imported (the Danbooru
    /// API's).
    pub fn has_notes_and_pools(self) -> bool {
        self == Kind::Danbooru
    }
}

/// Where the next page starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// The first page.
    Start,
    /// Posts older than this one.
    Before(i64),
    /// This page number (1-based).
    Page(u32),
}

/// Login details some sites want (Gelbooru's `api_key` and `user_id`,
/// Danbooru's `login` and `api_key`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    pub login: String,
    pub api_key: String,
}

fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

/// The URL of a page of posts matching `tags`.
pub fn page_url(
    kind: Kind,
    base: &str,
    tags: &str,
    cursor: Cursor,
    credentials: &Credentials,
) -> String {
    let base = base.trim_end_matches('/');
    let tags = encode(tags);
    let mut url = match (kind, cursor) {
        (Kind::Danbooru | Kind::E621, Cursor::Before(id)) => {
            format!("{base}/posts.json?tags={tags}&limit={PAGE_SIZE}&page=b{id}")
        }
        (Kind::Danbooru | Kind::E621, _) => {
            format!("{base}/posts.json?tags={tags}&limit={PAGE_SIZE}")
        }
        (Kind::Gelbooru, cursor) => {
            let pid = match cursor {
                Cursor::Page(n) => n.saturating_sub(1),
                _ => 0,
            };
            format!(
                "{base}/index.php?page=dapi&s=post&q=index&json=1&tags={tags}&limit={PAGE_SIZE}&pid={pid}"
            )
        }
        (Kind::Moebooru, cursor) => {
            let page = match cursor {
                Cursor::Page(n) => n,
                _ => 1,
            };
            format!("{base}/post.json?tags={tags}&limit={PAGE_SIZE}&page={page}")
        }
    };
    if !credentials.login.is_empty() && !credentials.api_key.is_empty() {
        let (login, key) = (encode(&credentials.login), encode(&credentials.api_key));
        match kind {
            Kind::Gelbooru => url.push_str(&format!("&user_id={login}&api_key={key}")),
            Kind::Danbooru | Kind::E621 => url.push_str(&format!("&login={login}&api_key={key}")),
            Kind::Moebooru => url.push_str(&format!("&login={login}&password_hash={key}")),
        }
    }
    url
}

/// The page after one that ended with `posts`, or `None` at the end.
pub fn next_cursor(kind: Kind, current: Cursor, posts: &[RemotePost]) -> Option<Cursor> {
    if posts.is_empty() {
        return None;
    }
    if kind.pages_by_id() {
        return posts.iter().map(|p| p.id).min().map(Cursor::Before);
    }
    Some(match current {
        Cursor::Page(n) => Cursor::Page(n + 1),
        _ => Cursor::Page(2),
    })
}

/// A post on the other site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePost {
    pub id: i64,
    /// Missing when the site hides the file (Danbooru's gold-only posts,
    /// e621's logged-out explicit posts).
    pub file_url: Option<String>,
    /// Lowercase hex, when the site says.
    pub md5: Option<String>,
    pub rating: Rating,
    /// As the upload form takes them: `name` or `category:name`.
    pub tags: Vec<String>,
    /// The post's own source, or its page on the other site.
    pub source: String,
    /// Its page on the other site.
    pub page_url: String,
    pub parent_id: Option<i64>,
    pub has_notes: bool,
}

fn rating(value: &str) -> Rating {
    match value.to_lowercase().as_str() {
        "g" | "general" | "s" | "safe" => Rating::General,
        "sensitive" => Rating::Sensitive,
        "q" | "questionable" => Rating::Questionable,
        _ => Rating::Explicit,
    }
}

/// Danbooru's `s` is sensitive, not safe as elsewhere.
fn danbooru_rating(value: &str) -> Rating {
    match value {
        "s" => Rating::Sensitive,
        other => rating(other),
    }
}

fn words(value: &Value) -> Vec<String> {
    value
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Tags with a category prefix for every category but general.
fn categorized(groups: &[(&str, Vec<String>)]) -> Vec<String> {
    let mut tags = Vec::new();
    for (category, names) in groups {
        for name in names {
            tags.push(if category.is_empty() {
                name.clone()
            } else {
                format!("{category}:{name}")
            });
        }
    }
    tags
}

/// Parses a page of posts from `body` (the site's JSON).
pub fn parse_page(kind: Kind, base: &str, body: &str) -> Result<Vec<RemotePost>, String> {
    let base = base.trim_end_matches('/');
    let json: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let items: Vec<Value> = match kind {
        Kind::E621 => json["posts"].as_array().cloned(),
        // An empty Gelbooru result has no "post" at all.
        Kind::Gelbooru => Some(
            json.get("post")
                .or_else(|| json.as_array().map(|_| &json))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        ),
        Kind::Danbooru | Kind::Moebooru => json.as_array().cloned(),
    }
    .ok_or_else(|| {
        format!(
            "unexpected answer: {}",
            body.chars().take(200).collect::<String>()
        )
    })?;
    let mut posts = Vec::with_capacity(items.len());
    for item in items {
        let Some(id) = item["id"].as_i64() else {
            continue;
        };
        let page_url = match kind {
            Kind::Danbooru | Kind::E621 => format!("{base}/posts/{id}"),
            Kind::Gelbooru => format!("{base}/index.php?page=post&s=view&id={id}"),
            Kind::Moebooru => format!("{base}/post/show/{id}"),
        };
        let post = match kind {
            Kind::Danbooru => RemotePost {
                id,
                file_url: text(&item["file_url"]),
                md5: text(&item["md5"]),
                rating: danbooru_rating(item["rating"].as_str().unwrap_or("e")),
                tags: categorized(&[
                    ("artist", words(&item["tag_string_artist"])),
                    ("copyright", words(&item["tag_string_copyright"])),
                    ("character", words(&item["tag_string_character"])),
                    ("meta", words(&item["tag_string_meta"])),
                    ("", words(&item["tag_string_general"])),
                ]),
                source: text(&item["source"]).unwrap_or_else(|| page_url.clone()),
                page_url,
                parent_id: item["parent_id"].as_i64(),
                has_notes: item["last_noted_at"].is_string(),
            },
            Kind::E621 => {
                let group = |name: &str| -> Vec<String> {
                    item["tags"][name]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let mut general = group("general");
                general.extend(group("species"));
                let mut meta = group("meta");
                meta.extend(group("lore"));
                RemotePost {
                    id,
                    file_url: text(&item["file"]["url"]),
                    md5: text(&item["file"]["md5"]),
                    rating: rating(item["rating"].as_str().unwrap_or("e")),
                    tags: categorized(&[
                        ("artist", group("artist")),
                        ("copyright", group("copyright")),
                        ("character", group("character")),
                        ("meta", meta),
                        ("", general),
                    ]),
                    source: item["sources"]
                        .as_array()
                        .and_then(|s| s.first())
                        .and_then(|s| s.as_str())
                        .map_or_else(|| page_url.clone(), str::to_owned),
                    page_url,
                    parent_id: item["relationships"]["parent_id"].as_i64(),
                    has_notes: false,
                }
            }
            Kind::Gelbooru | Kind::Moebooru => RemotePost {
                id,
                file_url: text(&item["file_url"]),
                md5: text(&item["md5"]),
                rating: rating(item["rating"].as_str().unwrap_or("e")),
                tags: words(&item["tags"]),
                source: text(&item["source"]).unwrap_or_else(|| page_url.clone()),
                page_url,
                parent_id: item["parent_id"].as_i64(),
                has_notes: false,
            },
        };
        posts.push(post);
    }
    Ok(posts)
}

/// A note on the other site, from Danbooru's `/notes.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteNote {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub body: String,
}

pub fn notes_url(base: &str, post_id: i64) -> String {
    format!(
        "{}/notes.json?search[post_id]={post_id}&search[is_active]=true&limit=1000",
        base.trim_end_matches('/')
    )
}

pub fn parse_notes(body: &str) -> Result<Vec<RemoteNote>, String> {
    let json: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let int = |v: &Value| v.as_i64().and_then(|n| i32::try_from(n).ok());
    Ok(json
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|n| n["is_active"].as_bool() != Some(false))
                .filter_map(|n| {
                    Some(RemoteNote {
                        x: int(&n["x"])?,
                        y: int(&n["y"])?,
                        width: int(&n["width"])?,
                        height: int(&n["height"])?,
                        body: text(&n["body"])?,
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// A pool on the other site, from Danbooru's `/pools.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePool {
    pub name: String,
    pub description: String,
    pub category: String,
    pub post_ids: Vec<i64>,
}

pub fn pools_url(base: &str, post_id: i64) -> String {
    format!(
        "{}/pools.json?search[post_ids_include_any]={post_id}&limit=20",
        base.trim_end_matches('/')
    )
}

pub fn parse_pools(body: &str) -> Result<Vec<RemotePool>, String> {
    let json: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    Ok(json
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|p| p["is_deleted"].as_bool() != Some(true))
                .filter_map(|p| {
                    Some(RemotePool {
                        name: text(&p["name"])?,
                        description: text(&p["description"]).unwrap_or_default(),
                        category: text(&p["category"]).unwrap_or_else(|| "series".into()),
                        post_ids: p["post_ids"]
                            .as_array()
                            .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_urls() {
        let none = Credentials::default();
        assert_eq!(
            page_url(
                Kind::Danbooru,
                "https://d.example/",
                "cat solo",
                Cursor::Before(99),
                &none
            ),
            "https://d.example/posts.json?tags=cat+solo&limit=100&page=b99"
        );
        assert_eq!(
            page_url(
                Kind::Gelbooru,
                "https://g.example",
                "cat",
                Cursor::Page(3),
                &Credentials {
                    login: "7".into(),
                    api_key: "k".into()
                }
            ),
            "https://g.example/index.php?page=dapi&s=post&q=index&json=1&tags=cat&limit=100&pid=2&user_id=7&api_key=k"
        );
        assert_eq!(
            page_url(
                Kind::Moebooru,
                "https://m.example",
                "",
                Cursor::Start,
                &none
            ),
            "https://m.example/post.json?tags=&limit=100&page=1"
        );
        assert_eq!(Kind::guess("www.e621.net"), Some(Kind::E621));
        assert_eq!(Kind::guess("example.com"), None);
    }

    #[test]
    fn danbooru_posts() {
        let body = r#"[{"id": 12, "file_url": "https://cdn.example/a.png", "md5": "abc",
            "rating": "s", "tag_string_general": "cat solo", "tag_string_artist": "someone",
            "tag_string_copyright": "", "tag_string_character": "", "tag_string_meta": "highres",
            "source": "", "parent_id": 3, "last_noted_at": "2026-01-01T00:00:00Z"},
            {"id": 11, "rating": "e", "tag_string_general": "x", "source": "https://art.example/1"}]"#;
        let posts = parse_page(Kind::Danbooru, "https://d.example", body).unwrap();
        assert_eq!(
            posts[0],
            RemotePost {
                id: 12,
                file_url: Some("https://cdn.example/a.png".into()),
                md5: Some("abc".into()),
                rating: Rating::Sensitive,
                tags: vec![
                    "artist:someone".into(),
                    "meta:highres".into(),
                    "cat".into(),
                    "solo".into()
                ],
                source: "https://d.example/posts/12".into(),
                page_url: "https://d.example/posts/12".into(),
                parent_id: Some(3),
                has_notes: true,
            }
        );
        assert_eq!(posts[1].file_url, None);
        assert_eq!(posts[1].source, "https://art.example/1");
        assert_eq!(
            next_cursor(Kind::Danbooru, Cursor::Start, &posts),
            Some(Cursor::Before(11))
        );
        assert_eq!(next_cursor(Kind::Danbooru, Cursor::Start, &[]), None);
    }

    #[test]
    fn other_sites_posts() {
        let e621 = r#"{"posts": [{"id": 5, "file": {"url": "https://f.example/5.png", "md5": "m"},
            "rating": "s", "tags": {"general": ["fur"], "species": ["cat"], "artist": ["a"], "lore": ["l"]},
            "sources": ["https://src.example"], "relationships": {"parent_id": null}}]}"#;
        let posts = parse_page(Kind::E621, "https://e.example", e621).unwrap();
        assert_eq!(posts[0].rating, Rating::General);
        assert_eq!(posts[0].tags, ["artist:a", "meta:l", "fur", "cat"]);
        assert_eq!(posts[0].source, "https://src.example");

        let gelbooru = r#"{"@attributes": {"count": 1}, "post": [{"id": 9, "file_url": "https://g.example/9.jpg",
            "md5": "q", "rating": "questionable", "tags": " cat  solo ", "source": ""}]}"#;
        let posts = parse_page(Kind::Gelbooru, "https://g.example", gelbooru).unwrap();
        assert_eq!(
            (posts[0].rating, posts[0].tags.clone()),
            (Rating::Questionable, vec!["cat".into(), "solo".into()])
        );
        assert_eq!(
            posts[0].source,
            "https://g.example/index.php?page=post&s=view&id=9"
        );
        assert!(
            parse_page(
                Kind::Gelbooru,
                "https://g.example",
                r#"{"@attributes": {"count": 0}}"#
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            next_cursor(Kind::Gelbooru, Cursor::Page(2), &posts),
            Some(Cursor::Page(3))
        );

        let moebooru =
            r#"[{"id": 4, "file_url": "https://m.example/4.png", "rating": "s", "tags": "cat"}]"#;
        assert_eq!(
            parse_page(Kind::Moebooru, "https://m.example", moebooru).unwrap()[0].rating,
            Rating::General
        );
        assert!(parse_page(Kind::Danbooru, "x", "<html>").is_err());
    }

    #[test]
    fn notes_and_pools() {
        let notes = parse_notes(
            r#"[{"x": 1, "y": 2, "width": 3, "height": 4, "body": "Hi", "is_active": true},
            {"x": 1, "y": 2, "width": 3, "height": 4, "body": "Gone", "is_active": false}]"#,
        )
        .unwrap();
        assert_eq!(
            notes,
            [RemoteNote {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
                body: "Hi".into()
            }]
        );
        let pools = parse_pools(r#"[{"name": "My_Comic", "description": "", "category": "series", "post_ids": [3, 1]}]"#)
            .unwrap();
        assert_eq!(pools[0].post_ids, [3, 1]);
    }
}
