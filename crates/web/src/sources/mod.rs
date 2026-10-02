//! Understanding where an upload comes from: "source strategies" that
//! read a work's page for the best file to download, the artist and their
//! profiles, the site's tags and the artist's commentary.
//!
//! Which site a link is on, and its canonical page and profile, is
//! [`moekura_core::sites`]'s: each site whose works can be read has a
//! strategy here, which asks the site's public API or page. A site
//! without one still gets its link's file and canonical page, and any
//! other page's OpenGraph tags are read. What a lookup finds is cached for
//! a few minutes, since the upload form asks about the same link as it's
//! typed and again when it's sent. Nothing here is essential: a site that
//! changed or is down just means the link is downloaded as it is, without
//! extras. Sites that only show works to members can be given a login in
//! `[sources.logins]`.

mod apple_music;
mod arca_live;
mod art_station;
mod art_street;
mod artistree;
mod behance;
mod bilibili;
mod blogger;
mod bluesky;
mod boorus;
mod booth;
mod carrd;
mod ci_en;
mod dc_inside;
mod deviantart;
mod dotpict;
mod fanbox;
mod fandom;
mod fantia;
mod fc2;
mod fediverse;
mod foriio;
mod four_chan;
mod furaffinity;
mod galleria;
mod grafolio;
mod gumroad;
mod hentai_foundry;
pub(crate) mod html;
mod huajia;
mod huashijie;
mod imgur;
mod inkbunny;
mod itaku;
mod kofi;
mod lofter;
mod mihuashi;
mod minitokyo;
mod miyoushe;
mod my_portfolio;
mod naver;
mod newgrounds;
mod nico_seiga;
mod nijie;
mod note;
mod odaibako;
mod opengraph;
mod opensea;
mod patreon;
mod piapro;
mod pinterest;
mod pixiv;
mod pixiv_family;
mod plurk;
mod poipiku;
mod postype;
mod privatter;
mod reddit;
mod redgifs;
mod sites;
pub(crate) use sites::READ;
mod skeb;
mod tinami;
mod tistory;
mod toyhouse;
mod tumblr;
mod twitter;
mod vk;
mod weibo;
mod xfolio;
mod xiaohongshu;
mod yachiyo_room;
mod youtube;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use moekura_core::config::SourcesConfig;
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
    /// A work on `site` whose page is `page_url`, nothing else known yet.
    pub(crate) fn new(
        site: &'static moekura_core::sites::Site,
        page_url: impl Into<String>,
    ) -> Self {
        Self {
            site: site.name,
            page_url: page_url.into(),
            ..Self::default()
        }
    }

    /// The headers for downloading, as the fetcher takes them.
    pub fn header_pairs(&self) -> Vec<(&str, &str)> {
        self.headers.iter().map(|(n, v)| (*n, v.as_str())).collect()
    }
}

/// Reading JSON and pages for the strategies, logged in to the sites
/// `[sources.logins]` has logins for.
pub(crate) struct Http<'a> {
    fetcher: &'a Fetcher,
    logins: &'a SourcesConfig,
}

impl Http<'_> {
    /// `url` with its site's login added, and the headers to send.
    fn logged_in(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<(Url, Vec<(String, String)>), String> {
        let mut url = Url::parse(url).map_err(|e| e.to_string())?;
        let mut all: Vec<(String, String)> = headers
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect();
        if let Some(login) = url.host_str().and_then(|h| self.logins.login_for(h)) {
            if !login.query.is_empty() {
                url.query_pairs_mut().extend_pairs(&login.query);
            }
            if !login.cookie.is_empty() {
                all.push(("Cookie".into(), login.cookie.clone()));
            }
            all.extend(login.headers.iter().map(|(n, v)| (n.clone(), v.clone())));
        }
        Ok((url, all))
    }

    /// Whether there's a login for `host`'s site.
    pub fn has_login(&self, host: &str) -> bool {
        self.logins.login_for(host).is_some()
    }

    pub async fn text(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<(String, String), String> {
        let (url, headers) = self.logged_in(url, headers)?;
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let (content_type, body) = self.fetcher.get(&url, &headers, MAX_RESPONSE).await?;
        Ok((content_type, String::from_utf8_lossy(&body).into_owned()))
    }

    /// A page's HTML.
    pub async fn page(&self, url: &str, headers: &[(&str, &str)]) -> Result<String, String> {
        Ok(self.text(url, headers).await?.1)
    }

    pub async fn json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<serde_json::Value, String> {
        let (_, body) = self.text(url, headers).await?;
        serde_json::from_str(&body).map_err(|e| format!("unreadable answer: {e}"))
    }

    /// POSTs `body` as JSON and reads the JSON answer.
    pub async fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let (url, headers) = self.logged_in(url, headers)?;
        let mut headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        headers.push(("Content-Type", "application/json"));
        let (_, answer) = self
            .fetcher
            .post(&url, &headers, body.to_string().into_bytes(), MAX_RESPONSE)
            .await?;
        serde_json::from_slice(&answer).map_err(|e| format!("unreadable answer: {e}"))
    }

    /// POSTs a form and reads the answer as text.
    pub async fn post_form(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        form: &[(&str, &str)],
    ) -> Result<String, String> {
        let (url, headers) = self.logged_in(url, headers)?;
        let mut headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        headers.push(("Content-Type", "application/x-www-form-urlencoded"));
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let (_, answer) = self
            .fetcher
            .post(&url, &headers, body.into_bytes(), MAX_RESPONSE)
            .await?;
        Ok(String::from_utf8_lossy(&answer).into_owned())
    }

    /// The first of `urls` that exists (asked with HEAD requests): the
    /// best of a file's sizes when the site doesn't say which it has.
    pub async fn first_existing(
        &self,
        urls: &[String],
        headers: &[(&str, &str)],
    ) -> Option<String> {
        for url in urls {
            if let Ok(parsed) = Url::parse(url)
                && self.fetcher.exists(&parsed, headers).await
            {
                return Some(url.clone());
            }
        }
        None
    }

    /// The cookies `url` sets (`name=value`).
    pub async fn set_cookies(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<Vec<String>, String> {
        let (url, headers) = self.logged_in(url, headers)?;
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        self.fetcher.set_cookies(&url, &headers, None).await
    }

    /// POSTs nothing to `url` and reads the answer as text.
    pub async fn post_empty(&self, url: &str, headers: &[(&str, &str)]) -> Result<String, String> {
        self.post_form(url, headers, &[]).await
    }

    /// Where `url` leads after its redirects.
    pub async fn final_url(&self, url: &str, headers: &[(&str, &str)]) -> Result<Url, String> {
        let (url, headers) = self.logged_in(url, headers)?;
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        self.fetcher.final_url(&url, &headers).await
    }

    /// What another link says (a booru post's original source), looked up
    /// as one level deeper.
    pub fn nested<'b>(
        &'b self,
        url: &'b Url,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SourceInfo>, String>> + Send + 'b>> {
        Box::pin(find(self, url, 1))
    }

    /// What a short link's target says, at the short link's `depth`.
    pub fn redirected<'b>(
        &'b self,
        url: &'b Url,
        depth: u8,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SourceInfo>, String>> + Send + 'b>> {
        Box::pin(find(self, url, depth))
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

/// Whether `url` is a file whose work isn't known from the link alone:
/// what it says (`found`) names no page but the file itself.
fn is_bare_file(url: &str, found: Option<&SourceInfo>) -> bool {
    let known = moekura_core::sites::parse(url);
    let is_file = match &known {
        Some(known) => known.is_file,
        None => Url::parse(url).is_ok_and(|u| is_file_url(&u)),
    };
    is_file
        && known.as_ref().is_none_or(|k| k.page_url.is_none())
        && found.is_none_or(|info| info.page_url.is_empty() || info.page_url == url)
}

/// Which strategy handles `url`, if a site-specific one does.
enum Strategy {
    Pixiv(pixiv::Target),
    Twitter(twitter::Target),
    Bluesky(bluesky::Target),
    DeviantArt,
    Fanbox(fanbox::Target),
    Skeb(skeb::Target),
}

fn site(url: &Url) -> Option<Strategy> {
    pixiv::target(url)
        .map(Strategy::Pixiv)
        .or_else(|| twitter::target(url).map(Strategy::Twitter))
        .or_else(|| bluesky::target(url).map(Strategy::Bluesky))
        .or_else(|| deviantart::matches(url).then_some(Strategy::DeviantArt))
        .or_else(|| fanbox::target(url).map(Strategy::Fanbox))
        .or_else(|| skeb::target(url).map(Strategy::Skeb))
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

/// What `url` says. `depth` is 0 for the link itself and 1 for a link
/// found while reading it, which isn't followed further.
async fn find(http: &Http<'_>, url: &Url, depth: u8) -> Result<Option<SourceInfo>, String> {
    let found = match site(url) {
        Some(Strategy::Pixiv(target)) => pixiv::fetch(http, &target).await?,
        Some(Strategy::Twitter(target)) => twitter::fetch(http, &target).await?,
        Some(Strategy::Bluesky(target)) => bluesky::fetch(http, &target).await?,
        Some(Strategy::DeviantArt) => deviantart::fetch(http, url).await?,
        Some(Strategy::Fanbox(target)) => fanbox::fetch(http, &target).await?,
        Some(Strategy::Skeb(target)) => skeb::fetch(http, &target).await?,
        None => {
            if let Some(known) = moekura_core::sites::parse(url.as_str()) {
                return sites::fetch(http, &known, url, depth).await;
            }
            if is_file_url(url) {
                return Ok(None);
            }
            if let Some(note) = sites::other_misskey(http, url).await {
                return Ok(Some(note));
            }
            return opengraph::fetch(http, url).await;
        }
    };
    Ok(Some(found))
}

/// How long asking a source may take: uploads can wait on it.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Source lookups, with their own quick fetcher, the sites' logins and
/// the cache.
pub struct Sources {
    fetcher: Fetcher,
    logins: SourcesConfig,
    cache: SourceCache,
}

impl Sources {
    /// `allow_private` exists for tests against a local server.
    pub fn new(allow_private: bool, config: SourcesConfig) -> Self {
        Self {
            fetcher: Fetcher::new(LOOKUP_TIMEOUT, allow_private),
            logins: config,
            cache: SourceCache::default(),
        }
    }

    /// What `url`'s source says (see [`lookup`]).
    pub async fn lookup(&self, url: &str) -> Option<Arc<SourceInfo>> {
        let http = Http {
            fetcher: &self.fetcher,
            logins: &self.logins,
        };
        lookup(&http, &self.cache, url).await
    }

    /// What `url` says, or, when it's a bare file nothing names the work
    /// of, what `referer` (the page it was found on, from a bookmarklet)
    /// says, with `url` as the work's first file. The referring page
    /// counts when it's on the file's site, or lists the file.
    pub async fn lookup_from(&self, url: &str, referer: &str) -> Option<Arc<SourceInfo>> {
        let found = self.lookup(url).await;
        let referer = referer.trim();
        if referer.is_empty() || referer == url || !is_bare_file(url, found.as_deref()) {
            return found;
        }
        let page = self.lookup(referer).await?;
        let same_site = moekura_core::sites::site_of(url)
            .is_some_and(|site| moekura_core::sites::site_of(referer) == Some(site));
        if !same_site && !page.files.iter().any(|f| f == url) {
            return found;
        }
        let mut info = (*page).clone();
        info.files.retain(|f| f != url);
        info.files.insert(0, url.to_owned());
        // The file isn't necessarily the one the page's frames are for.
        info.ugoira_frames = None;
        Some(Arc::new(info))
    }

    /// Answers lookups of `url` with `info`, as if its page said so.
    #[cfg(test)]
    pub fn remember(&self, url: &str, info: SourceInfo) {
        self.cache.put(url, Some(Arc::new(info)));
    }
}

/// What `url`'s source says, if it's a page some strategy reads (cached).
/// Failures are logged and give `None`.
async fn lookup(http: &Http<'_>, cache: &SourceCache, url: &str) -> Option<Arc<SourceInfo>> {
    let url = url.trim();
    let parsed = Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))?;
    if let Some(cached) = cache.get(url) {
        return cached;
    }
    let found = match find(http, &parsed, 0).await {
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
    // HTML's indentation isn't text.
    for line in out.lines() {
        let line = line.trim();
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

/// An id that's a JSON string or number, as text.
pub(crate) fn id_of(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Tags from a list of names.
pub(crate) fn tags_named<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Vec<SourceTag> {
    names
        .into_iter()
        .map(|n| n.as_ref().trim().to_owned())
        .filter(|n| !n.is_empty())
        .map(|name| SourceTag {
            name,
            translation: None,
        })
        .collect()
}

/// The strings in a JSON array (or the strings at `key` in its objects).
pub(crate) fn strings(value: &serde_json::Value, key: Option<&str>) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| match key {
                    Some(key) => v[key].as_str(),
                    None => v.as_str(),
                })
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
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
                Some(Strategy::Pixiv(_)) => "pixiv",
                Some(Strategy::Twitter(_)) => "twitter",
                Some(Strategy::Bluesky(_)) => "bluesky",
                Some(Strategy::DeviantArt) => "deviantart",
                Some(Strategy::Fanbox(_)) => "fanbox",
                Some(Strategy::Skeb(_)) => "skeb",
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
        let logins = SourcesConfig::default();
        let http = Http {
            fetcher: &fetcher,
            logins: &logins,
        };
        let cache = SourceCache::default();
        let url = format!("http://{addr}/work");
        let info = lookup(&http, &cache, &url).await.unwrap();
        assert_eq!(info.files, [format!("http://{addr}/big.png")]);
        assert_eq!(info.title, "A cat & a dog");
        assert_eq!(info.description, "Drawn today");
        assert_eq!(info.page_url, url);
        // Cached: the same answer without asking.
        assert!(Arc::ptr_eq(
            &info,
            &lookup(&http, &cache, &url).await.unwrap()
        ));
        // Files aren't pages.
        assert!(
            lookup(&http, &cache, &format!("http://{addr}/a.png"))
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
        state.sources = Arc::new(Sources::new(true, SourcesConfig::default()));
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
