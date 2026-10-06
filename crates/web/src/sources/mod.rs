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
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use moekura_core::config::{SiteLogin, SourcesConfig};
use moekura_core::permissions::Permission;
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use tracing::Instrument;
use url::Url;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::fetch::Fetcher;

/// How long a lookup is reused.
const CACHE_TTL: Duration = Duration::from_secs(10 * 60);
/// Lookups kept at once.
const CACHE_SIZE: usize = 500;
/// About how much memory the kept lookups may take, all together.
const CACHE_BYTES: usize = 32 * 1024 * 1024;
/// The most read from an API or page.
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
/// Lookups asking other servers at once; more wait their turn.
const IN_FLIGHT: usize = 16;
/// How long a lookup waits for its turn before giving up.
const TURN_WAIT: Duration = Duration::from_secs(10);
/// How long a whole lookup may take, all its requests together.
const LOOKUP_DEADLINE: Duration = Duration::from_secs(20);
/// HEAD requests one lookup may make to find a file's best size.
const MAX_PROBES: usize = 40;
/// Lookups using the X login, on this server: this many at once, then
/// one per [`X_LOGIN_PERIOD`] (30 every 15 minutes).
const X_LOGIN_BURST: u32 = 30;
const X_LOGIN_PERIOD: Duration = Duration::from_secs(30);
/// X's domains, whose requests carry the X login only when
/// [`Http::may_use_x_login`] says so.
const X_DOMAINS: &[&str] = &["x.com", "twitter.com"];

/// What's kept of a lookup (see [`SourceInfo::clamped`]): a page can say
/// a great deal, and what it says is cached.
const MAX_FILES: usize = 200;
const MAX_PROFILES: usize = 20;
const MAX_HEADERS: usize = 8;
const MAX_TAGS: usize = 200;
/// The longest URL kept, in bytes.
const MAX_URL: usize = 4096;
/// The longest name (an artist's, a tag's, a frame's file), in bytes.
const MAX_NAME: usize = 256;
/// An ugoira's most frames, as `moekura_media` takes them.
const MAX_FRAMES: usize = 2000;

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
    /// When the work was published, and last changed, as the site says.
    pub published_at: Option<OffsetDateTime>,
    pub updated_at: Option<OffsetDateTime>,
}

/// A date the way sites' APIs give them (RFC 3339, `2024-05-01T12:00:00Z`).
pub(crate) fn date(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text.trim(), &time::format_description::well_known::Rfc3339).ok()
}

/// `text` cut to its first `max` characters.
fn truncate_chars(text: &mut String, max: usize) {
    if let Some((at, _)) = text.char_indices().nth(max) {
        text.truncate(at);
    }
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

    /// What's worth keeping: no more files, profiles and tags than the
    /// `MAX_` constants say, no longer URLs and names (those are left
    /// out), and the title and description as long as a commentary's.
    /// `None` when the work's page is too long an address to be one.
    fn clamped(mut self) -> Option<Self> {
        if self.page_url.len() > MAX_URL {
            return None;
        }
        let short_url = |url: &String| url.len() <= MAX_URL;
        self.files.retain(short_url);
        self.files.truncate(MAX_FILES);
        self.headers.retain(|(_, value)| value.len() <= MAX_URL);
        self.headers.truncate(MAX_HEADERS);
        self.profile_urls.retain(short_url);
        self.profile_urls.truncate(MAX_PROFILES);
        let short_name = |name: &String| name.len() <= MAX_NAME;
        self.artist_name = self.artist_name.filter(short_name);
        self.artist_account = self.artist_account.filter(short_name);
        self.tags.retain(|tag| short_name(&tag.name));
        self.tags.truncate(MAX_TAGS);
        for tag in &mut self.tags {
            tag.translation = tag.translation.take().filter(short_name);
        }
        truncate_chars(&mut self.title, crate::commentary::TITLE_MAX_LEN);
        truncate_chars(
            &mut self.description,
            crate::commentary::DESCRIPTION_MAX_LEN,
        );
        self.ugoira_frames = self.ugoira_frames.filter(|frames| {
            frames.len() <= MAX_FRAMES && frames.iter().all(|(file, _)| short_name(file))
        });
        // What was read may have left the strings far larger than they are.
        for text in [&mut self.page_url, &mut self.title, &mut self.description]
            .into_iter()
            .chain(&mut self.files)
            .chain(&mut self.profile_urls)
        {
            text.shrink_to_fit();
        }
        self.files.shrink_to_fit();
        self.tags.shrink_to_fit();
        self.profile_urls.shrink_to_fit();
        Some(self)
    }

    /// About how many bytes it takes up.
    fn size(&self) -> usize {
        const ITEM: usize = 48;
        let texts = [&self.page_url, &self.title, &self.description]
            .into_iter()
            .chain(&self.files)
            .chain(&self.profile_urls)
            .chain(self.artist_name.iter())
            .chain(self.artist_account.iter())
            .map(|t| t.len() + ITEM)
            .sum::<usize>();
        let tags = self
            .tags
            .iter()
            .map(|t| t.name.len() + t.translation.as_ref().map_or(0, String::len) + ITEM)
            .sum::<usize>();
        let headers = self
            .headers
            .iter()
            .map(|(_, v)| v.len() + ITEM)
            .sum::<usize>();
        let frames = self
            .ugoira_frames
            .iter()
            .flatten()
            .map(|(f, _)| f.len() + ITEM)
            .sum::<usize>();
        std::mem::size_of::<Self>() + texts + tags + headers + frames
    }
}

/// Who a lookup is for. The X login (`[sources.logins."x.com"]`) only
/// reads posts for members who can upload: X suspends accounts it finds
/// reading it for anyone who asks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Asker {
    /// A signed-in member who can upload: who the upload paths, which
    /// check that first, look links up for.
    #[default]
    Uploader,
    /// Anyone else: a visitor, or a member who can't upload.
    Visitor,
}

impl Asker {
    pub fn of(current: &CurrentUser) -> Self {
        if current.user.is_some() && current.can(Permission::Upload) {
            Self::Uploader
        } else {
            Self::Visitor
        }
    }
}

/// A request as it's sent: its address and headers, with its site's
/// login when it has one.
struct Request {
    url: Url,
    headers: Vec<(String, String)>,
    /// Whether it carries a login.
    logged_in: bool,
}

/// Reading JSON and pages for the strategies, logged in to the sites
/// `[sources.logins]` has logins for, for one lookup.
pub(crate) struct Http<'a> {
    sources: &'a Sources,
    asker: Asker,
    /// HEAD requests this lookup may still make.
    probes: AtomicUsize,
    /// Whether this lookup may use the X login, once that's been asked.
    x_login: OnceLock<Result<(), &'static str>>,
    /// Whether the X login was left out (for a visitor, or with its
    /// allowance used up), so an uploader asking later should look again.
    partial: AtomicBool,
    /// The addresses asked with a login, for tests to check.
    #[cfg(test)]
    logged_in_to: Mutex<Vec<String>>,
}

impl<'a> Http<'a> {
    fn new(sources: &'a Sources, asker: Asker) -> Self {
        Self {
            sources,
            asker,
            probes: AtomicUsize::new(MAX_PROBES),
            x_login: OnceLock::new(),
            partial: AtomicBool::new(false),
            #[cfg(test)]
            logged_in_to: Mutex::default(),
        }
    }

    /// A request for `url` with `headers`, and its site's login if it has
    /// one and the request goes over https (and, for X's, if
    /// [`Self::may_use_x_login`]).
    fn logged_in(&self, url: &str, headers: &[(&str, &str)]) -> Result<Request, String> {
        let mut url = Url::parse(url).map_err(|e| e.to_string())?;
        let mut all: Vec<(String, String)> = headers
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect();
        let host = url.host_str().unwrap_or_default().to_owned();
        // Never in the clear.
        let login = (url.scheme() == "https")
            .then(|| self.sources.logins.login_for(&host))
            .flatten()
            .filter(|login| !self.is_x_login(&host, login) || self.may_use_x_login().is_ok());
        #[cfg(test)]
        if login.is_some() {
            self.logged_in_to
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(url.to_string());
        }
        if let Some(login) = login {
            if !login.query.is_empty() {
                url.query_pairs_mut().extend_pairs(&login.query);
            }
            if !login.cookie.is_empty() {
                all.push(("Cookie".into(), login.cookie.clone()));
            }
            all.extend(login.headers.iter().map(|(n, v)| (n.clone(), v.clone())));
        }
        Ok(Request {
            url,
            headers: all,
            logged_in: login.is_some(),
        })
    }

    /// The fetcher for a request: one that stays on the site when it
    /// carries a login.
    fn fetcher(&self, logged_in: bool) -> &Fetcher {
        if logged_in {
            &self.sources.logged_in
        } else {
            &self.sources.fetcher
        }
    }

    /// Whether there's a login for `host`'s site.
    pub fn has_login(&self, host: &str) -> bool {
        self.sources.logins.login_for(host).is_some()
    }

    /// Whether `login`, found for `host`, is X's: whatever X's own
    /// domains carry, and the login given for them wherever it's sent.
    fn is_x_login(&self, host: &str, login: &SiteLogin) -> bool {
        X_DOMAINS.iter().any(|domain| {
            host == *domain
                || host.ends_with(&format!(".{domain}"))
                || self
                    .sources
                    .logins
                    .login_for(domain)
                    .is_some_and(|x| std::ptr::eq(x, login))
        })
    }

    /// Whether the X login may be used for this lookup (why not, if not).
    /// The first time it's asked, it counts against the site-wide
    /// allowance when it may; the lookup's other requests to X go by the
    /// same answer.
    fn may_use_x_login(&self) -> Result<(), &'static str> {
        *self.x_login.get_or_init(|| {
            let refused = if self.asker != Asker::Uploader {
                "only for uploaders"
            } else if self.sources.x_logins.check().is_err() {
                "its allowance is used up for now"
            } else {
                return Ok(());
            };
            self.partial.store(true, Ordering::Relaxed);
            Err(refused)
        })
    }

    pub async fn text(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<(String, String), String> {
        let Request {
            url,
            headers,
            logged_in,
        } = self.logged_in(url, headers)?;
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let (content_type, body) = self
            .fetcher(logged_in)
            .get(&url, &headers, MAX_RESPONSE)
            .await?;
        Ok((content_type, String::from_utf8_lossy(&body).into_owned()))
    }

    /// [`Self::text`] without a login, for an address someone gave as it
    /// is: a login is for the strategies' own requests, not whatever page
    /// on its site a link names.
    pub async fn text_without_login(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<(String, String), String> {
        let url = Url::parse(url).map_err(|e| e.to_string())?;
        let (content_type, body) = self
            .sources
            .fetcher
            .get(&url, headers, MAX_RESPONSE)
            .await?;
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
        let Request {
            url,
            headers,
            logged_in,
        } = self.logged_in(url, headers)?;
        let mut headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        headers.push(("Content-Type", "application/json"));
        let (_, answer) = self
            .fetcher(logged_in)
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
        let Request {
            url,
            headers,
            logged_in,
        } = self.logged_in(url, headers)?;
        let mut headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        headers.push(("Content-Type", "application/x-www-form-urlencoded"));
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let (_, answer) = self
            .fetcher(logged_in)
            .post(&url, &headers, body.into_bytes(), MAX_RESPONSE)
            .await?;
        Ok(String::from_utf8_lossy(&answer).into_owned())
    }

    /// The first of `urls` that exists (asked with HEAD requests): the
    /// best of a file's sizes when the site doesn't say which it has.
    /// `None` too once the lookup has made [`MAX_PROBES`] of them: the
    /// files come from the page, which may list any number.
    pub async fn first_existing(
        &self,
        urls: &[String],
        headers: &[(&str, &str)],
    ) -> Option<String> {
        for url in urls {
            // Renamed `try_update` in 1.95, past the minimum supported Rust.
            #[allow(deprecated)]
            let left = self
                .probes
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
            if left.is_err() {
                return None;
            }
            if let Ok(parsed) = Url::parse(url)
                && self.sources.fetcher.exists(&parsed, headers).await
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
        let Request {
            url,
            headers,
            logged_in,
        } = self.logged_in(url, headers)?;
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        self.fetcher(logged_in)
            .set_cookies(&url, &headers, None)
            .await
    }

    /// POSTs nothing to `url` and reads the answer as text.
    pub async fn post_empty(&self, url: &str, headers: &[(&str, &str)]) -> Result<String, String> {
        self.post_form(url, headers, &[]).await
    }

    /// Where `url` leads after its redirects.
    pub async fn final_url(&self, url: &str, headers: &[(&str, &str)]) -> Result<Url, String> {
        let Request {
            url,
            headers,
            logged_in,
        } = self.logged_in(url, headers)?;
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        self.fetcher(logged_in).final_url(&url, &headers).await
    }

    /// [`Self::final_url`] without a login, for an address someone gave
    /// as it is (see [`Self::text_without_login`]).
    pub async fn final_url_without_login(&self, url: &str) -> Result<Url, String> {
        let url = Url::parse(url).map_err(|e| e.to_string())?;
        self.sources.fetcher.final_url(&url, &[]).await
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

/// A lookup's answer, when it was made, and whether an uploader should
/// look again (see [`Http::partial`]).
struct Cached {
    at: Instant,
    info: Option<Arc<SourceInfo>>,
    partial: bool,
    bytes: usize,
}

#[derive(Default)]
struct Entries {
    by_url: HashMap<String, Cached>,
    /// What they all take up, about.
    bytes: usize,
}

impl Entries {
    fn remove(&mut self, url: &str) {
        if let Some(gone) = self.by_url.remove(url) {
            self.bytes -= gone.bytes;
        }
    }
}

/// Remembers lookups for [`CACHE_TTL`], at most [`CACHE_SIZE`] of them
/// in about [`CACHE_BYTES`].
#[derive(Default)]
pub struct SourceCache {
    entries: Mutex<Entries>,
}

impl SourceCache {
    fn get(&self, url: &str, asker: Asker) -> Option<Option<Arc<SourceInfo>>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.by_url.get(url)?;
        if entry.at.elapsed() >= CACHE_TTL {
            entries.remove(url);
            return None;
        }
        if entry.partial && asker == Asker::Uploader {
            return None;
        }
        Some(entry.info.clone())
    }

    fn put(&self, url: &str, info: Option<Arc<SourceInfo>>, partial: bool) {
        let bytes = url.len() + info.as_deref().map_or(0, SourceInfo::size);
        if bytes > CACHE_BYTES {
            return;
        }
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(url);
        let expired: Vec<String> = entries
            .by_url
            .iter()
            .filter(|(_, e)| e.at.elapsed() >= CACHE_TTL)
            .map(|(url, _)| url.clone())
            .collect();
        for url in expired {
            entries.remove(&url);
        }
        // The oldest go first.
        while entries.by_url.len() >= CACHE_SIZE || entries.bytes + bytes > CACHE_BYTES {
            let Some(oldest) = entries
                .by_url
                .iter()
                .min_by_key(|(_, e)| e.at)
                .map(|(url, _)| url.clone())
            else {
                break;
            };
            entries.remove(&oldest);
        }
        entries.bytes += bytes;
        entries.by_url.insert(
            url.to_owned(),
            Cached {
                at: Instant::now(),
                info,
                partial,
                bytes,
            },
        );
    }

    fn contains(&self, url: &str, asker: Asker) -> bool {
        self.get(url, asker).is_some()
    }

    fn forget(&self, url: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(url);
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

/// What to tell an uploader whose link couldn't be read, when it's a
/// page that would be no use downloaded as it is (a post on X).
pub(crate) fn unread_message(url: &str) -> Option<&'static str> {
    let url = Url::parse(url.trim()).ok()?;
    match site(&url)? {
        Strategy::Twitter(_) => Some(twitter::UNREAD),
        _ => None,
    }
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

/// Whether `piece` is `.` or `..`, as a URL parser reads it (`%2e` is a
/// dot too, and tabs and line breaks are dropped).
fn is_dot_segment(piece: &str) -> bool {
    let piece: String = piece
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect::<String>()
        .to_ascii_lowercase()
        .replace("%2e", ".");
    piece == "." || piece == ".."
}

/// Whether `text` has a `%` escape in it, which another decoding (when
/// it's put into an address) would turn into something else.
fn has_escape(text: &str) -> bool {
    text.as_bytes()
        .windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit())
}

/// Whether `url`'s decoded path segments or query values could move a
/// request built from them somewhere else on the site: the sites'
/// canonical pages, and the strategies' API addresses, are made from
/// them. A segment mustn't hide a separator (`/`, `\`, `?`, `#`), an
/// escape or a control character, or be a dot segment; a query value
/// (which may be a link of its own) mustn't climb out with dot segments,
/// backslashes or control characters.
fn hides_path(url: &Url) -> bool {
    let decoded = |text: &str| {
        percent_encoding::percent_decode_str(text)
            .decode_utf8_lossy()
            .into_owned()
    };
    let bad_segment = |segment: &str| {
        segment.contains(['/', '\\', '?', '#'])
            || segment.chars().any(char::is_control)
            || has_escape(segment)
            || is_dot_segment(segment)
    };
    let bad_value = |value: &str| {
        value.contains('\\')
            || value.chars().any(char::is_control)
            || value.split('/').any(is_dot_segment)
    };
    url.path_segments()
        .into_iter()
        .flatten()
        .any(|segment| bad_segment(&decoded(segment)))
        || url.query_pairs().any(|(_, value)| bad_value(&value))
}

/// Whether `page`, the work's page [`moekura_core::sites`] made from a
/// link, is the page a link to it gives back: the strategies read it,
/// and ask the site's API, with the site's login. A page made from a
/// crafted id, query value or link inside a link (`?blogId=logout%3F`,
/// `?h5url=…`) isn't the work's, but wherever on the site the id led.
fn is_canonical(page: &str) -> bool {
    Url::parse(page).is_ok_and(|url| !hides_path(&url))
        && moekura_core::sites::parse(page)
            .and_then(|known| known.page_url)
            .is_some_and(|again| again == page)
}

/// What `url` says. `depth` is 0 for the link itself and 1 for a link
/// found while reading it, which isn't followed further.
async fn find(http: &Http<'_>, url: &Url, depth: u8) -> Result<Option<SourceInfo>, String> {
    if hides_path(url) {
        return Err("the link hides separators or dot segments in its path".into());
    }
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

/// Whether a request to `first` that carries a login may follow a
/// redirect to `next`: over https, to a site the same login is for.
fn same_login(logins: &SourcesConfig, first: &Url, next: &Url) -> bool {
    let login = |url: &Url| url.host_str().and_then(|h| logins.login_for(h));
    next.scheme() == "https"
        && match (login(first), login(next)) {
            (Some(was), Some(is)) => std::ptr::eq(was, is),
            _ => false,
        }
}

/// How long asking a source may take: uploads can wait on it.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Source lookups, with their own quick fetchers, the sites' logins and
/// the cache.
pub struct Sources {
    fetcher: Fetcher,
    /// For requests carrying a login: they only follow redirects within
    /// the login's site (see [`same_login`]).
    logged_in: Fetcher,
    logins: SourcesConfig,
    cache: SourceCache,
    /// Turns to ask other servers (see [`IN_FLIGHT`]).
    in_flight: Arc<Semaphore>,
    /// Lookups that may still use the X login (see [`X_LOGIN_BURST`]).
    x_logins: DefaultDirectRateLimiter,
}

impl Sources {
    /// `allow_private` exists for tests against a local server.
    pub fn new(allow_private: bool, config: SourcesConfig) -> Self {
        let x_quota = Quota::with_period(X_LOGIN_PERIOD)
            .expect("period is non-zero")
            .allow_burst(NonZeroU32::new(X_LOGIN_BURST).expect("burst is non-zero"));
        let logins = config.clone();
        Self {
            fetcher: Fetcher::new(LOOKUP_TIMEOUT, allow_private),
            logged_in: Fetcher::with_redirects(
                LOOKUP_TIMEOUT,
                allow_private,
                move |first, next| same_login(&logins, first, next),
            ),
            logins: config,
            cache: SourceCache::default(),
            in_flight: Arc::new(Semaphore::new(IN_FLIGHT)),
            x_logins: RateLimiter::direct(x_quota),
        }
    }

    /// What `url`'s source says, for an uploader (see [`Self::lookup_as`]).
    pub async fn lookup(self: &Arc<Self>, url: &str) -> Option<Arc<SourceInfo>> {
        self.lookup_as(url, Asker::Uploader).await
    }

    /// What `url`'s source says, if it's a page some strategy reads,
    /// looked up for `asker` (cached). Failures are logged and give
    /// `None`.
    ///
    /// The lookup runs on a blocking thread, as reading a page takes
    /// time, and holds one of [`IN_FLIGHT`] turns until it's done, even
    /// if whoever asked has gone.
    pub async fn lookup_as(self: &Arc<Self>, url: &str, asker: Asker) -> Option<Arc<SourceInfo>> {
        let url = url.trim();
        let parsed = Url::parse(url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))?;
        if let Some(cached) = self.cache.get(url, asker) {
            return cached;
        }
        let turn = tokio::time::timeout(TURN_WAIT, Arc::clone(&self.in_flight).acquire_owned());
        let Ok(Ok(turn)) = turn.await else {
            tracing::warn!(url, "too many sources are being read; not reading this one");
            return None;
        };
        let this = Arc::clone(self);
        let url = url.to_owned();
        let runtime = tokio::runtime::Handle::current();
        let span = tracing::Span::current();
        tokio::task::spawn_blocking(move || {
            let _turn = turn;
            runtime.block_on(this.read(&url, &parsed, asker).instrument(span))
        })
        .await
        .ok()?
    }

    /// Reads what `url` says and caches it.
    async fn read(&self, url: &str, parsed: &Url, asker: Asker) -> Option<Arc<SourceInfo>> {
        let http = Http::new(self, asker);
        let found = match tokio::time::timeout(LOOKUP_DEADLINE, find(&http, parsed, 0)).await {
            Ok(Ok(found)) => found.and_then(SourceInfo::clamped).map(Arc::new),
            Ok(Err(error)) => {
                tracing::info!(url, error, "could not read the source");
                None
            }
            Err(_) => {
                tracing::info!(url, "reading the source took too long");
                None
            }
        };
        self.cache
            .put(url, found.clone(), http.partial.load(Ordering::Relaxed));
        found
    }

    /// Whether a lookup of `url` for `asker` would be answered from the
    /// cache, without asking another server.
    pub(crate) fn is_cached(&self, url: &str, asker: Asker) -> bool {
        self.cache.contains(url.trim(), asker)
    }

    /// What `url` says, or, when it's a bare file nothing names the work
    /// of, what `referer` (the page it was found on, from a bookmarklet)
    /// says, with `url` as the work's first file. The referring page
    /// counts when it's on the file's site, or lists the file. For an
    /// uploader.
    pub async fn lookup_from(
        self: &Arc<Self>,
        url: &str,
        referer: &str,
    ) -> Option<Arc<SourceInfo>> {
        self.lookup_from_as(url, referer, Asker::Uploader).await
    }

    /// [`Self::lookup_from`] for `asker`.
    pub async fn lookup_from_as(
        self: &Arc<Self>,
        url: &str,
        referer: &str,
        asker: Asker,
    ) -> Option<Arc<SourceInfo>> {
        let found = self.lookup_as(url, asker).await;
        let referer = referer.trim();
        if referer.is_empty() || referer == url || !is_bare_file(url, found.as_deref()) {
            return found;
        }
        let page = self.lookup_as(referer, asker).await?;
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
        self.cache.put(url, Some(Arc::new(info)), false);
    }

    /// Answers lookups of `url` with nothing, as if its page couldn't be read.
    #[cfg(test)]
    pub fn remember_unread(&self, url: &str) {
        self.cache.put(url, None, false);
    }
}

/// Whether `current` may have a source looked up again rather than see
/// what was found a few minutes ago: users whose uploads skip the
/// approval queue. Others would make every request ask the site.
pub(crate) fn may_refresh(current: &CurrentUser) -> bool {
    current.can(Permission::UploadWithoutApproval)
}

/// What `url`'s source says, for a page `current` opened from `ip`: a
/// lookup that asks another server counts against their allowance
/// (`TooManyRequests` once it's used up), and the X login is only used
/// for uploaders. `refresh` looks again rather than reuse what was found,
/// for those who [`may_refresh`].
pub(crate) async fn lookup_for_page(
    state: &AppState,
    current: &CurrentUser,
    ip: Option<IpAddr>,
    url: &str,
    refresh: bool,
) -> Result<Option<Arc<SourceInfo>>, AppError> {
    let url = url.trim();
    let asker = Asker::of(current);
    let is_link = Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
    let refresh = refresh && may_refresh(current);
    if is_link && (refresh || !state.sources.is_cached(url, asker)) {
        let client = crate::rate_limit::client_key(current.user.as_ref().map(|u| u.id), ip);
        state.rate_limits.check_source_lookup(&client).await?;
    }
    if refresh {
        state.sources.cache.forget(url);
    }
    Ok(state.sources.lookup_as(url, asker).await)
}

/// [`lookup_for_page`] for a page that can go without what the source
/// says: nothing, rather than `TooManyRequests`, once `current`'s lookups
/// are used up for now.
pub(crate) async fn lookup_for_panel(
    state: &AppState,
    current: &CurrentUser,
    ip: Option<IpAddr>,
    url: &str,
    refresh: bool,
) -> Result<Option<Arc<SourceInfo>>, AppError> {
    match lookup_for_page(state, current, ip, url, refresh).await {
        Err(AppError::TooManyRequests { .. }) => Ok(None),
        found => found,
    }
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
        // An entity is at most 10 bytes before its `;`: looking further
        // would read the rest of the text for every `&`.
        let window = &after.as_bytes()[..after.len().min(11)];
        let end = window.iter().position(|&b| b == b';');
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

/// `id` if it's a number (ASCII digits), as a work's id in a link should
/// be before it goes into a request's address.
pub(crate) fn number(id: &str) -> Result<&str, String> {
    (!id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit()))
        .then_some(id)
        .ok_or_else(|| "the link's id isn't a number".into())
}

/// `id` if it's a plain name or key (ASCII letters and digits, `-`, `_`
/// and `.`, not starting with a dot), as an id or a user's name in a link
/// should be before it goes into a request's address. Those characters
/// stand for themselves there, so it stays one path segment or query
/// value.
pub(crate) fn key(id: &str) -> Result<&str, String> {
    (!id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
    .then_some(id)
    .ok_or_else(|| "the link's id isn't one".into())
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

/// Tags from a list of names, no more than are kept (see
/// [`SourceInfo::clamped`]).
pub(crate) fn tags_named<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Vec<SourceTag> {
    names
        .into_iter()
        .map(|n| n.as_ref().trim().to_owned())
        .filter(|n| !n.is_empty())
        .take(MAX_TAGS)
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

    /// A local server for `app`, and its address.
    async fn serve(app: axum::Router) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }

    /// Sources that may read a local server, with `logins`.
    fn local_sources(logins: SourcesConfig) -> Arc<Sources> {
        Arc::new(Sources::new(true, logins))
    }

    /// A login with a cookie and an API key for `domain`.
    fn login_for(domain: &str) -> SourcesConfig {
        let mut config = SourcesConfig::default();
        config.logins.insert(
            domain.to_owned(),
            moekura_core::config::SiteLogin {
                cookie: "session=secret".into(),
                query: [("api_key".to_owned(), "key".to_owned())].into(),
                headers: [("X-Api-Key".to_owned(), "key".to_owned())].into(),
            },
        );
        config
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
        let addr = serve(Router::new().route(
            "/work",
            get(move || async move { axum::response::Html(page) }),
        ))
        .await;

        let sources = local_sources(SourcesConfig::default());
        let url = format!("http://{addr}/work");
        let info = sources.lookup(&url).await.unwrap();
        assert_eq!(info.files, [format!("http://{addr}/big.png")]);
        assert_eq!(info.title, "A cat & a dog");
        assert_eq!(info.description, "Drawn today");
        assert_eq!(info.page_url, url);
        // Cached: the same answer without asking.
        assert!(sources.is_cached(&url, Asker::Visitor));
        assert!(Arc::ptr_eq(&info, &sources.lookup(&url).await.unwrap()));
        // Files aren't pages.
        assert!(
            sources
                .lookup(&format!("http://{addr}/a.png"))
                .await
                .is_none()
        );
    }

    #[test]
    fn entities_decode_in_one_pass() {
        assert_eq!(
            decode_entities("&amp;&lt;&#x3042;&#12354;&nbsp;&bogus; & ; &#xffffffff;"),
            "&<ああ &bogus; & ; &#xffffffff;"
        );
        // An entity is at most 10 bytes before its `;`.
        assert_eq!(decode_entities("&#000000065;"), "A");
        assert_eq!(decode_entities("&#0000000065;"), "&#0000000065;");
        // Every `&` used to look for a `;` to the end of the text.
        let text = "&".repeat(4 * 1024 * 1024);
        let started = Instant::now();
        assert_eq!(decode_entities(&text), text);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn lookups_are_clamped() {
        let long = "x".repeat(MAX_URL + 1);
        let info = SourceInfo {
            page_url: "https://example.com/work".into(),
            files: [long.clone()]
                .into_iter()
                .chain((0..MAX_FILES + 50).map(|i| format!("https://example.com/{i}.png")))
                .collect(),
            artist_name: Some("n".repeat(MAX_NAME + 1)),
            artist_account: Some("artist".into()),
            profile_urls: vec![long.clone(), "https://example.com/artist".into()],
            tags: (0..MAX_TAGS * 2)
                .map(|i| SourceTag {
                    name: format!("tag{i}"),
                    translation: Some("t".repeat(MAX_NAME + 1)),
                })
                .collect(),
            title: "t".repeat(crate::commentary::TITLE_MAX_LEN * 2),
            description: "猫".repeat(crate::commentary::DESCRIPTION_MAX_LEN * 2),
            ugoira_frames: Some(vec![("0.jpg".into(), 100); MAX_FRAMES + 1]),
            ..SourceInfo::default()
        };
        let kept = info.clamped().unwrap();
        assert_eq!(kept.files.len(), MAX_FILES);
        assert_eq!(kept.files[0], "https://example.com/0.png");
        assert_eq!(kept.artist_name, None);
        assert_eq!(kept.artist_account.as_deref(), Some("artist"));
        assert_eq!(kept.profile_urls, ["https://example.com/artist"]);
        assert_eq!(kept.tags.len(), MAX_TAGS);
        assert!(kept.tags.iter().all(|t| t.translation.is_none()));
        assert_eq!(kept.title.chars().count(), crate::commentary::TITLE_MAX_LEN);
        assert_eq!(
            kept.description.chars().count(),
            crate::commentary::DESCRIPTION_MAX_LEN
        );
        assert_eq!(kept.ugoira_frames, None);
        assert!(kept.size() < 1024 * 1024, "{}", kept.size());
        // A page that long isn't one.
        let info = SourceInfo {
            page_url: long,
            ..SourceInfo::default()
        };
        assert_eq!(info.clamped(), None);
    }

    #[test]
    fn the_cache_forgets_and_keeps_to_its_budget() {
        let cache = SourceCache::default();
        let big = SourceInfo {
            description: "x".repeat(CACHE_BYTES / 4),
            ..SourceInfo::default()
        };
        for i in 0..10 {
            cache.put(
                &format!("https://example.com/{i}"),
                Some(Arc::new(big.clone())),
                false,
            );
        }
        {
            let entries = cache.entries.lock().unwrap();
            assert!(entries.bytes <= CACHE_BYTES);
            assert!(entries.by_url.len() < 4);
            assert_eq!(
                entries.bytes,
                entries.by_url.values().map(|e| e.bytes).sum::<usize>()
            );
        }
        // The newest stay.
        assert!(cache.get("https://example.com/9", Asker::Visitor).is_some());
        assert!(cache.get("https://example.com/0", Asker::Visitor).is_none());

        // An expired entry is dropped when it's asked for.
        let old = Instant::now().checked_sub(CACHE_TTL + Duration::from_secs(1));
        if let Some(old) = old {
            cache
                .entries
                .lock()
                .unwrap()
                .by_url
                .get_mut("https://example.com/9")
                .unwrap()
                .at = old;
            assert!(cache.get("https://example.com/9", Asker::Visitor).is_none());
            assert!(
                !cache
                    .entries
                    .lock()
                    .unwrap()
                    .by_url
                    .contains_key("https://example.com/9")
            );
        }

        // What a visitor was told without the X login is no answer for an
        // uploader.
        cache.put("https://x.com/a/status/1", None, true);
        assert!(
            cache
                .get("https://x.com/a/status/1", Asker::Visitor)
                .is_some()
        );
        assert!(
            cache
                .get("https://x.com/a/status/1", Asker::Uploader)
                .is_none()
        );
    }

    #[test]
    fn links_hiding_separators_are_refused() {
        let hides = |u: &str| hides_path(&Url::parse(u).unwrap());
        for fine in [
            "https://www.pixiv.net/en/artworks/123",
            "https://x.com/artist/status/1?s=20",
            "https://fantia.jp/posts/2245222",
            "https://example.com/wiki/100%25_Orange",
            "https://example.com/%E7%8C%AB",
            "https://pixiv.net/?url=https://example.com/a/b.png",
        ] {
            assert!(!hides(fine), "{fine}");
        }
        for crafted in [
            "https://fantia.jp/posts/..%5C..%5Clogout",
            "https://fantia.jp/posts/1%2F..%2F..%2Flogout",
            "https://fantia.jp/posts/1%3Fa%3Db",
            "https://fantia.jp/posts/1%23x",
            "https://fantia.jp/posts/%252e%252e",
            "https://fantia.jp/posts/.%09.",
            "https://blog.naver.com/PostView.naver?blogId=..%2F..%2Flogout&logNo=1",
            "https://example.com/a?u=%5Clogout",
        ] {
            assert!(hides(crafted), "{crafted}");
        }
    }

    #[test]
    fn strategies_only_read_canonical_pages() {
        let page = |link: &str| {
            moekura_core::sites::parse(link)
                .and_then(|known| known.page_url)
                .unwrap_or_else(|| panic!("{link} isn't a work"))
        };
        for link in [
            "https://blog.naver.com/PostView.naver?blogId=cat&logNo=223",
            "https://galleria.emotionflow.com/IllustDetailV.jsp?ID=1&TD=2",
            "https://gall.dcinside.com/mgallery/board/view/?id=cat&no=1",
            "http://a.blog.fc2.com/?mode=image&filename=b.jpg",
            "https://www.patreon.com/file?h=123",
            "https://fantia.jp/posts/2245222",
            "https://caswac1.tistory.com/entry/용사의-선택지가-이상하다",
        ] {
            assert!(is_canonical(&page(link)), "{link}");
        }
        // Query values, and links inside links, that leave the work's
        // page once they're put in it. They get past `hides_path`.
        let nested = "https://a.lofter.com/post/1%2F..%2F..%2Flogout";
        let nested: String = url::form_urlencoded::byte_serialize(nested.as_bytes()).collect();
        for crafted in [
            "https://blog.naver.com/PostView.naver?blogId=logout%3F&logNo=1",
            "https://galleria.emotionflow.com/IllustDetailV.jsp?ID=account%2Fsettings%3F&TD=1",
            "https://gall.dcinside.com/mgallery/board/view/?id=cat%26x%3D1&no=1",
            "http://a.blog.fc2.com/?mode=image&filename=a/b?c",
            "https://www.patreon.com/file?h=logout%3Fx",
            &format!("https://uls.lofter.com/?h5url={nested}"),
        ] {
            assert!(!hides_path(&Url::parse(crafted).unwrap()), "{crafted}");
            let page = page(crafted);
            assert!(!is_canonical(&page), "{crafted} gave {page}");
        }
    }

    #[tokio::test]
    async fn strategies_check_ids_before_asking() {
        assert_eq!(number("123"), Ok("123"));
        assert!(number("12a").is_err() && number("").is_err());
        assert_eq!(key("cat-art_1.x"), Ok("cat-art_1.x"));
        for bad in [
            "", ".", "..", ".hidden", "a/b", "a\\b", "a?b", "a#b", "a%2fb", "a b",
        ] {
            assert!(key(bad).is_err(), "{bad}");
        }

        // Pages a strategy is given that name no valid work are refused
        // before any request (nothing listens on these).
        let sources = local_sources(SourcesConfig::default());
        let http = Http::new(&sources, Asker::Uploader);
        let known = |page: &str| moekura_core::sites::parse(page).unwrap();
        let fails = |result: Result<SourceInfo, String>| {
            let error = result.unwrap_err();
            assert!(error.contains("isn't") || error.contains("not "), "{error}");
        };
        let page = "https://fantia.jp/posts/1/../../logout";
        fails(fantia::fetch(&http, &known("https://fantia.jp/posts/1"), page).await);
        let page = "https://pawoo.net/@a/1x";
        fails(fediverse::mastodon(&http, &known("https://pawoo.net/@a/1"), page).await);
        let page = "https://arca.live/b/x/1&a=b";
        fails(arca_live::fetch(&http, &known("https://arca.live/b/x/1"), page).await);
        let page = "https://boards.4chan.org/a%2Fb/thread/1";
        fails(four_chan::fetch(&http, &known("https://boards.4chan.org/a/thread/1"), page).await);
        let tumblr = "https://artist.tumblr.com/post/1";
        fails(tumblr::fetch(&http, &known(tumblr), "https://artist.tumblr.com/post/1?x").await);
        let dc = "https://gall.dcinside.com/mgallery/board/view/?id=cat&no=1";
        fails(
            dc_inside::fetch(
                &http,
                &known(dc),
                "https://gall.dcinside.com/mgallery/board/view/?id=cat&x=1&no=1",
            )
            .await,
        );
        let hf = "https://www.hentai-foundry.com/pictures/user/a/1";
        fails(
            hentai_foundry::fetch(&http, &known(hf), "https://www.hentai-foundry.com/pic-1/x")
                .await,
        );
        let naver = "https://blog.naver.com/cat/123";
        fails(naver::blog(&http, &known(naver), "https://blog.naver.com/logout?a=/123").await);
        let galleria = "https://galleria.emotionflow.com/1/2.html";
        fails(
            galleria::fetch(
                &http,
                &known(galleria),
                "https://galleria.emotionflow.com/logout?x=/2.html",
            )
            .await,
        );
        // An album's file name, from the link's query.
        let fc2 = "http://a.blog.fc2.com/?mode=image&filename=a/b?c";
        assert_eq!(
            known(fc2).page_url.as_deref(),
            Some("http://a.blog.fc2.com/img/a/b?c/")
        );
        fails(fc2::fetch(&http, &known(fc2), "http://a.blog.fc2.com/img/a/b?c/").await);
        let note = "https://www.xiaohongshu.com/explore/65880524000000000700a643";
        fails(xiaohongshu::fetch(&http, &known(note), &format!("{note}?xsec_token=a&b=c")).await);
        let post = "https://www.youtube.com/post/Ugkx1";
        fails(
            youtube::fetch(
                &http,
                &known(post),
                "https://www.youtube.com/post/logout?x=1",
            )
            .await,
        );
        // A crafted link is refused as a whole.
        assert!(
            sources
                .lookup("https://fantia.jp/posts/..%5C..%5Clogout")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn logins_go_only_over_https_and_stay_on_their_site() {
        use axum::Router;
        use axum::response::Redirect;
        use axum::routing::get;

        let sources = local_sources(login_for("example.com"));
        let http = Http::new(&sources, Asker::Uploader);
        let sent = http
            .logged_in("https://www.example.com/a", &[("Accept", "x")])
            .unwrap();
        assert!(sent.logged_in);
        assert_eq!(sent.url.as_str(), "https://www.example.com/a?api_key=key");
        assert!(
            sent.headers
                .contains(&("Cookie".into(), "session=secret".into()))
        );
        assert!(sent.headers.contains(&("X-Api-Key".into(), "key".into())));
        // Never in the clear.
        let sent = http.logged_in("http://example.com/a", &[]).unwrap();
        assert!(!sent.logged_in);
        assert_eq!(sent.url.as_str(), "http://example.com/a");
        assert!(sent.headers.is_empty());

        let logins = login_for("example.com");
        let url = |u: &str| Url::parse(u).unwrap();
        let first = url("https://example.com/a");
        assert!(same_login(
            &logins,
            &first,
            &url("https://www.example.com/b")
        ));
        assert!(!same_login(&logins, &first, &url("http://example.com/b")));
        assert!(!same_login(
            &logins,
            &first,
            &url("https://elsewhere.net/b")
        ));

        // A request kept on its site isn't sent where a redirect leads
        // elsewhere (127.0.0.1 and localhost are different hosts), but
        // can still say where that is.
        let addr = serve(Router::new().route(
            "/go",
            get(|axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>| async move {
                Redirect::temporary(&q["to"])
            }),
        ))
        .await;
        let fetcher = Fetcher::with_redirects(Duration::from_secs(5), true, |first, next| {
            first.host_str() == next.host_str()
        });
        let away = format!("http://localhost:{}/elsewhere", addr.port());
        let go = url(&format!("http://{addr}/go?to={away}"));
        let error = fetcher.get(&go, &[], 1024).await.unwrap_err();
        assert!(error.contains("307"), "{error}");
        assert_eq!(fetcher.final_url(&go, &[]).await.unwrap().as_str(), away);
    }

    #[tokio::test]
    async fn probes_have_a_budget() {
        use std::sync::atomic::AtomicUsize;

        use axum::Router;
        use axum::routing::head;

        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let addr = serve(Router::new().route(
            "/{file}",
            head(move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                    axum::http::StatusCode::NOT_FOUND
                }
            }),
        ))
        .await;
        let sources = local_sources(SourcesConfig::default());
        let http = Http::new(&sources, Asker::Uploader);
        let files: Vec<String> = (0..MAX_PROBES * 2)
            .map(|i| format!("http://{addr}/{i}.jpg"))
            .collect();
        assert_eq!(http.first_existing(&files, &[]).await, None);
        assert_eq!(http.first_existing(&files, &[]).await, None);
        assert_eq!(asked.load(Ordering::Relaxed), MAX_PROBES);
    }

    #[tokio::test]
    async fn lookups_take_turns() {
        use axum::Router;
        use axum::routing::get;

        let page = r#"<meta property="og:image" content="/art.png">"#;
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let addr = serve(Router::new().route(
            "/{work}",
            get(move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                    axum::response::Html(page)
                }
            }),
        ))
        .await;
        let sources = local_sources(SourcesConfig::default());
        let taken = Arc::clone(&sources.in_flight)
            .acquire_many_owned(IN_FLIGHT as u32)
            .await
            .unwrap();
        let waiting = tokio::spawn({
            let sources = Arc::clone(&sources);
            let url = format!("http://{addr}/1");
            async move { sources.lookup(&url).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!waiting.is_finished());
        assert_eq!(asked.load(Ordering::Relaxed), 0);
        drop(taken);
        assert!(waiting.await.unwrap().is_some());
        assert_eq!(asked.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_x_login_is_for_uploaders_within_its_allowance() {
        let mut logins = login_for("x.com");
        logins.logins.insert(
            "example.com".into(),
            moekura_core::config::SiteLogin {
                cookie: "other=secret".into(),
                ..Default::default()
            },
        );
        let sources = Sources::new(true, logins);
        let carries_it = |http: &Http<'_>, url: &str| {
            let sent = http.logged_in(url, &[]).unwrap();
            let cookie = sent.headers.iter().any(|(name, _)| name == "Cookie");
            assert_eq!(sent.logged_in, cookie, "{url}");
            cookie
        };

        // Whatever asks X for a visitor goes without it: a link to a post
        // X's strategy reads, or a page of X's read as it is.
        let visitor = Http::new(&sources, Asker::Visitor);
        assert!(!carries_it(&visitor, "https://x.com/i/web/status/1"));
        assert!(visitor.partial.load(Ordering::Relaxed));
        assert_eq!(visitor.may_use_x_login(), Err("only for uploaders"));
        assert!(carries_it(&visitor, "https://example.com/work"));

        // An uploader's lookup counts once, however often it asks X.
        for _ in 0..X_LOGIN_BURST {
            let uploader = Http::new(&sources, Asker::Uploader);
            assert_eq!(uploader.may_use_x_login(), Ok(()));
            assert!(carries_it(&uploader, "https://x.com/i/api/graphql/q"));
            assert!(carries_it(&uploader, "https://api.x.com/1/a"));
            assert!(!uploader.partial.load(Ordering::Relaxed));
        }
        let uploader = Http::new(&sources, Asker::Uploader);
        assert!(!carries_it(&uploader, "https://x.com/a/status/1"));
        assert_eq!(
            uploader.may_use_x_login(),
            Err("its allowance is used up for now")
        );
        assert!(uploader.partial.load(Ordering::Relaxed));
        assert!(carries_it(&uploader, "https://example.com/work"));
    }

    #[tokio::test]
    async fn pages_a_strategy_refuses_are_read_without_the_login() {
        // A server standing in for the site's: it doesn't speak https, so
        // requests to it fail, but only once they're made.
        let addr = serve(axum::Router::new()).await;
        let sources = local_sources(login_for("127.0.0.1"));
        let http = Http::new(&sources, Asker::Uploader);
        // A blog name that leaves the post's address once it's put in.
        let link = "https://blog.naver.com/PostView.naver?blogId=logout%3F&logNo=1";
        let link = Url::parse(link).unwrap();
        assert!(!hides_path(&link));
        let mut known = moekura_core::sites::parse(link.as_str()).unwrap();
        assert_eq!(
            known.page_url.as_deref(),
            Some("https://blog.naver.com/logout?/1")
        );
        known.page_url = Some(format!("https://{addr}/logout?/1"));
        let info = sites::fetch(&http, &known, &link, 0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(info.page_url, format!("https://{addr}/logout?/1"));
        assert!(info.files.is_empty());
        assert!(http.logged_in_to.lock().unwrap().is_empty());
        // A strategy's own requests carry it.
        let _ = http.page(&format!("https://{addr}/api"), &[]).await;
        assert_eq!(
            *http.logged_in_to.lock().unwrap(),
            [format!("https://{addr}/api")]
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn page_lookups_count_against_an_allowance(pool: sqlx::PgPool) {
        use axum::Router;
        use axum::http::StatusCode;
        use axum::routing::get;
        use moekura_core::permissions::SystemRole;

        use crate::test_support::{TestApp, current_user, session_for, test_state};

        let page = r#"<meta property="og:image" content="/art.png">"#;
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let addr = serve(Router::new().route(
            "/{work}",
            get(move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                    axum::response::Html(page)
                }
            }),
        ))
        .await;
        let mut state = test_state(&pool).await;
        state.sources = local_sources(SourcesConfig::default());
        let visitor = crate::auth::tests_support_visitor(&state);
        let ip = Some(IpAddr::from([198, 51, 100, 9]));

        // A visitor's lookups that ask the site count; cached ones don't.
        for i in 0..10 {
            let url = format!("http://{addr}/{i}");
            let found = lookup_for_page(&state, &visitor, ip, &url, false).await;
            assert!(found.unwrap().is_some());
            let again = lookup_for_page(&state, &visitor, ip, &url, true).await;
            assert!(again.unwrap().is_some(), "refreshing isn't for visitors");
        }
        assert_eq!(asked.load(Ordering::Relaxed), 10);
        let url = format!("http://{addr}/10");
        let refused = lookup_for_page(&state, &visitor, ip, &url, false).await;
        assert!(matches!(refused, Err(AppError::TooManyRequests { .. })));
        assert_eq!(asked.load(Ordering::Relaxed), 10);
        // Another address has its own allowance; what isn't a link is free.
        let other = Some(IpAddr::from([198, 51, 100, 10]));
        let url = format!("http://{addr}/11");
        assert!(
            lookup_for_page(&state, &visitor, other, &url, false)
                .await
                .is_ok()
        );
        for _ in 0..20 {
            let none = lookup_for_page(&state, &visitor, ip, "not a link", false).await;
            assert!(none.unwrap().is_none());
        }

        // Members look again only when their uploads skip the queue.
        let member = current_user(
            &state,
            &session_for(&pool, "alice", SystemRole::Member).await,
        )
        .await;
        let contributor = current_user(
            &state,
            &session_for(&pool, "bob", SystemRole::Contributor).await,
        )
        .await;
        assert_eq!(Asker::of(&visitor), Asker::Visitor);
        assert_eq!(Asker::of(&member), Asker::Uploader);
        let url = format!("http://{addr}/0");
        let before = asked.load(Ordering::Relaxed);
        lookup_for_page(&state, &member, None, &url, false)
            .await
            .unwrap();
        lookup_for_page(&state, &member, None, &url, true)
            .await
            .unwrap();
        assert_eq!(asked.load(Ordering::Relaxed), before);
        lookup_for_page(&state, &contributor, None, &url, true)
            .await
            .unwrap();
        assert_eq!(asked.load(Ordering::Relaxed), before + 1);

        // Through the pages: the finder says so, the related tags go
        // without the source's.
        let app = TestApp::new(
            state.clone(),
            crate::artists::routes().merge(crate::related_tags::routes()),
        );
        let mut finder = Vec::new();
        for i in 20..32 {
            let query: String =
                url::form_urlencoded::byte_serialize(format!("http://{addr}/{i}").as_bytes())
                    .collect();
            finder.push(
                app.get_json(&format!("/artists/finder?url={query}"), None)
                    .await
                    .status,
            );
            let related = app
                .get_json(&format!("/tags/related?tags=cat&source={query}"), None)
                .await;
            assert_eq!(related.status, StatusCode::OK, "{}", related.body);
        }
        assert!(
            finder[..10].iter().all(|s| *s == StatusCode::OK),
            "{finder:?}"
        );
        assert_eq!(finder[11], StatusCode::TOO_MANY_REQUESTS);
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
