//! Understanding where an upload comes from: "source strategies" that
//! read a work's page on Pixiv, X, Bluesky, DeviantArt, Fanbox or Skeb
//! (and OpenGraph tags anywhere else) for the best file to download, the
//! artist and their profiles, the site's tags and the artist's
//! commentary.
//!
//! Each strategy recognises its site's URLs and asks the site's public
//! API or page; what it finds is cached for a few minutes, since the
//! upload form asks about the same link as it's typed and again when it's
//! sent. Nothing here is essential: a site that changed or is down just
//! means the link is downloaded as it is, without extras.

mod bluesky;
mod deviantart;
mod fanbox;
mod opengraph;
mod pixiv;
mod skeb;
mod twitter;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use url::Url;

use crate::fetch::Fetcher;

/// How long a lookup is reused.
const CACHE_TTL: Duration = Duration::from_secs(10 * 60);
/// Lookups kept at once.
const CACHE_SIZE: usize = 500;
/// The most read from an API or page.
const MAX_RESPONSE: usize = 4 * 1024 * 1024;

/// A tag the source gave the work, with the site's own translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTag {
    pub name: String,
    pub translation: Option<String>,
}

/// What a source's page says about a work.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceInfo {
    /// The site's name, for people.
    pub site: &'static str,
    /// The work's page: what the post's source should be.
    pub page_url: String,
    /// The work's files, best first; the one the link pointed at (a page
    /// of several) comes first.
    pub files: Vec<String>,
    /// Headers downloading them needs (a `Referer`).
    pub headers: Vec<(&'static str, String)>,
    /// The artist's name on the site.
    pub artist_name: Option<String>,
    /// Their account's name (a handle), which makes a good tag.
    pub artist_account: Option<String>,
    /// The artist's pages there, which artist entries may list.
    pub profile_urls: Vec<String>,
    pub tags: Vec<SourceTag>,
    /// The artist's title and description of the work.
    pub title: String,
    pub description: String,
    /// For a Pixiv ugoira: each frame's file in the zip and delay (ms),
    /// which the zip itself lacks.
    pub ugoira_frames: Option<Vec<(String, u32)>>,
}

impl SourceInfo {
    /// The headers for downloading, as the fetcher takes them.
    pub fn header_pairs(&self) -> Vec<(&str, &str)> {
        self.headers.iter().map(|(n, v)| (*n, v.as_str())).collect()
    }
}

/// Reading JSON and pages for the strategies.
pub(crate) struct Http<'a> {
    fetcher: &'a Fetcher,
}

impl Http<'_> {
    pub async fn text(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<(String, String), String> {
        let url = Url::parse(url).map_err(|e| e.to_string())?;
        let (content_type, body) = self.fetcher.get(&url, headers, MAX_RESPONSE).await?;
        Ok((content_type, String::from_utf8_lossy(&body).into_owned()))
    }

    pub async fn json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<serde_json::Value, String> {
        let (_, body) = self.text(url, headers).await?;
        serde_json::from_str(&body).map_err(|e| format!("unreadable answer: {e}"))
    }
}

/// A lookup's answer and when it was made.
type Cached = (Instant, Option<Arc<SourceInfo>>);

/// Remembers lookups for [`CACHE_TTL`].
#[derive(Default)]
pub struct SourceCache {
    entries: Mutex<HashMap<String, Cached>>,
}

impl SourceCache {
    fn get(&self, url: &str) -> Option<Option<Arc<SourceInfo>>> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(url)
            .filter(|(at, _)| at.elapsed() < CACHE_TTL)
            .map(|(_, info)| info.clone())
    }

    fn put(&self, url: &str, info: Option<Arc<SourceInfo>>) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= CACHE_SIZE {
            entries.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
            if entries.len() >= CACHE_SIZE {
                entries.clear();
            }
        }
        entries.insert(url.to_owned(), (Instant::now(), info));
    }
}

/// Which strategy handles `url`, if a site-specific one does.
enum Site {
    Pixiv(pixiv::Target),
    Twitter(twitter::Target),
    Bluesky(bluesky::Target),
    DeviantArt,
    Fanbox(fanbox::Target),
    Skeb(skeb::Target),
}

fn site(url: &Url) -> Option<Site> {
    pixiv::target(url)
        .map(Site::Pixiv)
        .or_else(|| twitter::target(url).map(Site::Twitter))
        .or_else(|| bluesky::target(url).map(Site::Bluesky))
        .or_else(|| deviantart::matches(url).then_some(Site::DeviantArt))
        .or_else(|| fanbox::target(url).map(Site::Fanbox))
        .or_else(|| skeb::target(url).map(Site::Skeb))
}

/// Whether `url` looks like a file rather than a page.
fn is_file_url(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    [
        ".jpg", ".jpeg", ".png", ".gif", ".webp", ".avif", ".jxl", ".mp4", ".webm", ".zip",
    ]
    .iter()
    .any(|ext| path.ends_with(ext))
}

async fn find(fetcher: &Fetcher, url: &Url) -> Result<Option<SourceInfo>, String> {
    let http = Http { fetcher };
    let found = match site(url) {
        Some(Site::Pixiv(target)) => pixiv::fetch(&http, &target).await?,
        Some(Site::Twitter(target)) => twitter::fetch(&http, &target).await?,
        Some(Site::Bluesky(target)) => bluesky::fetch(&http, &target).await?,
        Some(Site::DeviantArt) => deviantart::fetch(&http, url).await?,
        Some(Site::Fanbox(target)) => fanbox::fetch(&http, &target).await?,
        Some(Site::Skeb(target)) => skeb::fetch(&http, &target).await?,
        None if is_file_url(url) => return Ok(None),
        None => return opengraph::fetch(&http, url).await,
    };
    Ok(Some(found))
}

/// How long asking a source may take: uploads can wait on it.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Source lookups, with their own quick fetcher and the cache.
pub struct Sources {
    fetcher: Fetcher,
    cache: SourceCache,
}

impl Sources {
    /// `allow_private` exists for tests against a local server.
    pub fn new(allow_private: bool) -> Self {
        Self {
            fetcher: Fetcher::new(LOOKUP_TIMEOUT, allow_private),
            cache: SourceCache::default(),
        }
    }

    /// What `url`'s source says (see [`lookup`]).
    pub async fn lookup(&self, url: &str) -> Option<Arc<SourceInfo>> {
        lookup(&self.fetcher, &self.cache, url).await
    }

    /// Answers lookups of `url` with `info`, as if its page said so.
    #[cfg(test)]
    pub fn remember(&self, url: &str, info: SourceInfo) {
        self.cache.put(url, Some(Arc::new(info)));
    }
}

/// What `url`'s source says, if it's a page some strategy reads (cached).
/// Failures are logged and give `None`.
pub async fn lookup(fetcher: &Fetcher, cache: &SourceCache, url: &str) -> Option<Arc<SourceInfo>> {
    let url = url.trim();
    let parsed = Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))?;
    if let Some(cached) = cache.get(url) {
        return cached;
    }
    let found = match find(fetcher, &parsed).await {
        Ok(found) => found.map(Arc::new),
        Err(error) => {
            tracing::info!(url, error, "could not read the source");
            None
        }
    };
    cache.put(url, found.clone());
    found
}

/// The artist entries (not deleted) of a source's work: those with its
/// page or one of the artist's profiles among their URLs.
pub(crate) async fn artists_for(
    db: &sqlx::PgPool,
    info: &SourceInfo,
) -> sqlx::Result<Vec<moekura_db::artists::Artist>> {
    let mut found: Vec<moekura_db::artists::Artist> = Vec::new();
    for url in std::iter::once(&info.page_url).chain(&info.profile_urls) {
        for artist in moekura_db::artists::find_by_url(db, url).await? {
            if !found.iter().any(|a| a.id == artist.id) {
                found.push(artist);
            }
        }
    }
    Ok(found)
}

/// The site's tags as this site's: tags whose wiki pages list one as an
/// other name, and tags named like one or its translation. Each with
/// the source's tag it comes from.
pub(crate) async fn translated_tags(
    db: &sqlx::PgPool,
    info: &SourceInfo,
) -> sqlx::Result<Vec<(moekura_db::tags::Tag, String)>> {
    use moekura_core::wiki::normalize_other_name;

    let mut out: Vec<(moekura_db::tags::Tag, String)> = Vec::new();
    let others: Vec<String> = info
        .tags
        .iter()
        .flat_map(|t| std::iter::once(&t.name).chain(&t.translation))
        .map(|n| normalize_other_name(n))
        .filter(|n| !n.is_empty())
        .collect();
    let pages = moekura_db::wiki::titles_for_other_names(db, &others).await?;
    let titles: Vec<&str> = pages.iter().map(|(t, _)| t.as_str()).collect();
    let by_wiki = moekura_db::tags::by_names(db, &titles).await?;
    for (title, names) in &pages {
        let (Some(tag), Some(from)) = (
            by_wiki.iter().find(|t| &t.name == title),
            info.tags.iter().find(|t| {
                names.contains(&normalize_other_name(&t.name))
                    || t.translation
                        .as_deref()
                        .is_some_and(|tr| names.contains(&normalize_other_name(tr)))
            }),
        ) else {
            continue;
        };
        if !out.iter().any(|(t, _)| t.id == tag.id) {
            out.push((tag.clone(), from.name.clone()));
        }
    }
    // Named alike: a translation, or a tag already in English.
    let named: Vec<(String, &SourceTag)> = info
        .tags
        .iter()
        .flat_map(|t| {
            std::iter::once(&t.name)
                .chain(&t.translation)
                .filter_map(move |n| {
                    moekura_core::tags::TagName::parse(n)
                        .ok()
                        .map(|n| (n.into_string(), t))
                })
        })
        .collect();
    let names: Vec<&str> = named.iter().map(|(n, _)| n.as_str()).collect();
    for tag in moekura_db::tags::by_names(db, &names).await? {
        if tag.post_count == 0 || out.iter().any(|(t, _)| t.id == tag.id) {
            continue;
        }
        if let Some((_, from)) = named.iter().find(|(n, _)| *n == tag.name) {
            out.push((tag, from.name.clone()));
        }
    }
    Ok(out)
}

/// Text from a bit of HTML: line breaks for `<br>` and paragraph ends,
/// no other tags, entities decoded.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&decode_entities(&rest[..start]));
        let Some(end) = rest[start..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[start + 1..start + end].trim().to_ascii_lowercase();
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or_default();
        if name == "br" || (tag.starts_with('/') && matches!(name, "p" | "div" | "li")) {
            out.push('\n');
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(&decode_entities(rest));
    // No runs of blank lines, no trailing spaces.
    let mut text = String::new();
    let mut blank = 0;
    for line in out.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(line);
        text.push('\n');
    }
    text.trim().to_owned()
}

/// `&amp;`, `&#39;`, `&#x27;` and the other usual entities decoded.
pub(crate) fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|end| {
            let entity = &after[..end];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A JSON string field, or empty.
pub(crate) fn text_of(value: &serde_json::Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_text() {
        assert_eq!(
            html_to_text("Hello<br />world &amp; <a href=\"x\">friends</a><br><br><br>Bye&#x21;"),
            "Hello\nworld & friends\n\nBye!"
        );
        assert_eq!(
            decode_entities("a &unknown; &#12354; b"),
            "a &unknown; あ b"
        );
    }

    #[test]
    fn sites_are_recognised() {
        let which = |u: &str| {
            let url = Url::parse(u).unwrap();
            match site(&url) {
                Some(Site::Pixiv(_)) => "pixiv",
                Some(Site::Twitter(_)) => "twitter",
                Some(Site::Bluesky(_)) => "bluesky",
                Some(Site::DeviantArt) => "deviantart",
                Some(Site::Fanbox(_)) => "fanbox",
                Some(Site::Skeb(_)) => "skeb",
                None => "none",
            }
        };
        assert_eq!(which("https://www.pixiv.net/en/artworks/123"), "pixiv");
        assert_eq!(which("https://x.com/artist/status/1/photo/2"), "twitter");
        assert_eq!(
            which("https://bsky.app/profile/a.bsky.social/post/3k"),
            "bluesky"
        );
        assert_eq!(
            which("https://www.deviantart.com/a/art/Cat-123"),
            "deviantart"
        );
        assert_eq!(which("https://artist.fanbox.cc/posts/55"), "fanbox");
        assert_eq!(which("https://skeb.jp/@artist/works/3"), "skeb");
        assert_eq!(which("https://example.com/a"), "none");
    }

    #[tokio::test]
    async fn opengraph_pages_and_the_cache() {
        use axum::Router;
        use axum::routing::get;

        let page = r#"<html><head>
            <meta property="og:title" content="A cat &amp; a dog">
            <meta property="og:image" content="/big.png">
            <meta name="description" content="Drawn today">
            </head></html>"#;
        let app = Router::new().route(
            "/work",
            get(move || async move { axum::response::Html(page) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let fetcher = Fetcher::new(Duration::from_secs(5), true);
        let cache = SourceCache::default();
        let url = format!("http://{addr}/work");
        let info = lookup(&fetcher, &cache, &url).await.unwrap();
        assert_eq!(info.files, [format!("http://{addr}/big.png")]);
        assert_eq!(info.title, "A cat & a dog");
        assert_eq!(info.description, "Drawn today");
        assert_eq!(info.page_url, url);
        // Cached: the same answer without asking.
        assert!(Arc::ptr_eq(
            &info,
            &lookup(&fetcher, &cache, &url).await.unwrap()
        ));
        // Files aren't pages.
        assert!(
            lookup(&fetcher, &cache, &format!("http://{addr}/a.png"))
                .await
                .is_none()
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn uploads_from_a_page_and_translates_tags(pool: sqlx::PgPool) {
        use axum::Router;
        use axum::routing::get;
        use moekura_core::permissions::SystemRole;

        use crate::test_support::{TestApp, fixture, session_for, test_state};

        let png = fixture::png(24, 24);
        let page = r#"<meta property="og:image" content="/art.png">
            <meta property="og:title" content="Cat"><meta property="og:description" content="New work">"#;
        let origin = Router::new()
            .route(
                "/work",
                get(move || async move { axum::response::Html(page) }),
            )
            .route("/art.png", get(move || async move { png }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let mut state = test_state(&pool).await;
        state.fetcher = Fetcher::new(Duration::from_secs(10), true);
        state.sources = Arc::new(Sources::new(true));
        let app = TestApp::new(
            state.clone(),
            crate::upload::routes(1024 * 1024 * 10).merge(crate::related_tags::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let link = format!("http://{addr}/work");
        let fields = vec![("url", link.clone()), ("rating", "g".to_owned())];
        let response = app
            .post_multipart("/upload", Some(&alice), &fields, None)
            .await;
        assert_eq!(
            response.status,
            axum::http::StatusCode::SEE_OTHER,
            "{}",
            response.body
        );
        let id: i64 = response.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.source, link, "the page, not the file, is the source");
        let commentary = moekura_db::artist_commentaries::for_post(&pool, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(commentary.texts.original_title, "Cat");
        assert_eq!(commentary.texts.original_description, "New work");

        // Tags a site gives are found through wiki other names and names.
        sqlx::query("INSERT INTO tags (name, post_count) VALUES ('cat', 3), ('original', 2)")
            .execute(&pool)
            .await
            .unwrap();
        moekura_db::wiki::save(
            &pool,
            "cat",
            moekura_db::wiki::Text {
                body: "A cat.",
                other_names: Some(&["猫".to_owned()]),
            },
            None,
            None,
        )
        .await
        .unwrap();
        let info = SourceInfo {
            tags: vec![
                SourceTag {
                    name: "猫".into(),
                    translation: None,
                },
                SourceTag {
                    name: "オリジナル".into(),
                    translation: Some("original".into()),
                },
                SourceTag {
                    name: "nothing".into(),
                    translation: None,
                },
            ],
            ..SourceInfo::default()
        };
        let found: Vec<(String, String)> = translated_tags(&pool, &info)
            .await
            .unwrap()
            .into_iter()
            .map(|(t, from)| (t.name, from))
            .collect();
        assert_eq!(
            found,
            [
                ("cat".to_owned(), "猫".to_owned()),
                ("original".to_owned(), "オリジナル".to_owned())
            ]
        );
    }
}
