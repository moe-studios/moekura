//! Recognising links to the sites art comes from, as Danbooru does: which
//! site a URL is on, whether it's a file, a work's page or an artist's
//! profile, and the canonical form of each. Reading what a page says is
//! the web crate's (its source strategies); this only reads URLs.
//!
//! The patterns follow Danbooru's (`app/logical/source/url/` in
//! danbooru/danbooru), one module per site or family of sites.

mod list;
mod parts;

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
mod deviant_art;
mod dotpict;
mod fanbox;
mod fandom;
mod fantia;
mod fc2;
mod foriio;
mod four_chan;
mod furaffinity;
mod galleria;
mod google;
mod grafolio;
mod gumroad;
mod hentai_foundry;
mod huajia;
mod huashijie;
mod imgur;
mod inkbunny;
mod itaku;
mod kakao;
mod kofi;
mod lofter;
mod mastodon;
mod mihuashi;
mod minitokyo;
mod misskey;
mod miyoushe;
mod my_portfolio;
mod naver;
mod newgrounds;
mod nico_seiga;
mod nijie;
mod note;
mod odaibako;
mod opensea;
mod patreon;
mod piapro;
mod pinterest;
mod pixiv;
mod pixiv_comic;
mod pixiv_factory;
mod pixiv_sketch;
mod plurk;
mod poipiku;
mod postype;
mod privatter;
mod reddit;
mod redgifs;
mod skeb;
mod skland;
mod tiktok;
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

pub use list::*;
use parts::Parts;

/// A site links can be recognised on.
#[derive(Debug, PartialEq, Eq)]
pub struct Site {
    /// A stable name for code: what strategies match on, and the site's
    /// icon's id.
    pub key: &'static str,
    /// The site's name, for people.
    pub name: &'static str,
    /// Its home page.
    pub url: &'static str,
}

/// What a link is, on a site we know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An image or video.
    File,
    /// A work's page (a post, an illustration, a status).
    Page,
    /// An artist's profile.
    Profile,
    /// Anything else on the site.
    Other,
}

/// A link on a known site, and its canonical forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceUrl {
    pub site: &'static Site,
    /// Whether the link is a file rather than a page.
    pub is_file: bool,
    /// For a file, its full-size version, when the link is a sample or
    /// thumbnail of it (or the link itself, when it's already that).
    pub file_url: Option<String>,
    /// For a file, whether it's known to be a resized copy (a sample or
    /// thumbnail) rather than the original, as Danbooru's
    /// `image_sample?` says.
    pub is_sample: bool,
    /// The work's page this link is (or, for a file, belongs to).
    pub page_url: Option<String>,
    /// The artist's profile this link is or belongs to.
    pub profile_url: Option<String>,
}

impl SourceUrl {
    fn of(site: &'static Site) -> Self {
        Self {
            site,
            is_file: false,
            file_url: None,
            is_sample: false,
            page_url: None,
            profile_url: None,
        }
    }

    /// Marks the link a file whose full version is `full`.
    fn file(mut self, full: impl Into<Option<String>>) -> Self {
        self.is_file = true;
        self.file_url = full.into();
        self
    }

    /// Marks the file a resized copy of the original, when `is_sample`.
    fn sample(mut self, is_sample: bool) -> Self {
        self.is_sample = is_sample;
        self
    }

    fn page(mut self, url: impl Into<Option<String>>) -> Self {
        self.page_url = url.into();
        self
    }

    fn profile(mut self, url: impl Into<Option<String>>) -> Self {
        self.profile_url = url.into();
        self
    }

    pub fn kind(&self) -> Kind {
        if self.is_file {
            Kind::File
        } else if self.page_url.is_some() {
            Kind::Page
        } else if self.profile_url.is_some() {
            Kind::Profile
        } else {
            Kind::Other
        }
    }
}

type Parser = fn(&Parts) -> Option<SourceUrl>;

/// Every site's parser. Each checks its own domains, so the order only
/// matters where sites share one (Pixiv's family).
const PARSERS: &[Parser] = &[
    pixiv_comic::parse,
    pixiv_factory::parse,
    pixiv_sketch::parse,
    booth::parse,
    fanbox::parse,
    pixiv::parse,
    twitter::parse,
    bluesky::parse,
    deviant_art::parse,
    skeb::parse,
    boorus::parse,
    misskey::parse,
    mastodon::parse,
    four_chan::parse,
    my_portfolio::parse,
    apple_music::parse,
    arca_live::parse,
    artistree::parse,
    art_station::parse,
    art_street::parse,
    behance::parse,
    bilibili::parse,
    blogger::parse,
    carrd::parse,
    ci_en::parse,
    dc_inside::parse,
    dotpict::parse,
    fandom::parse,
    fantia::parse,
    fc2::parse,
    foriio::parse,
    furaffinity::parse,
    galleria::parse,
    google::parse,
    grafolio::parse,
    gumroad::parse,
    hentai_foundry::parse,
    huajia::parse,
    huashijie::parse,
    imgur::parse,
    inkbunny::parse,
    itaku::parse,
    kakao::parse,
    kofi::parse,
    lofter::parse,
    mihuashi::parse,
    minitokyo::parse,
    miyoushe::parse,
    naver::parse,
    newgrounds::parse,
    nico_seiga::parse,
    nijie::parse,
    note::parse,
    odaibako::parse,
    opensea::parse,
    patreon::parse,
    piapro::parse,
    pinterest::parse,
    plurk::parse,
    poipiku::parse,
    postype::parse,
    privatter::parse,
    reddit::parse,
    redgifs::parse,
    skland::parse,
    tiktok::parse,
    tinami::parse,
    tistory::parse,
    toyhouse::parse,
    tumblr::parse,
    vk::parse,
    weibo::parse,
    xfolio::parse,
    xiaohongshu::parse,
    yachiyo_room::parse,
    youtube::parse,
];

/// What `raw` is, if it's an http(s) link on a site we know.
pub fn parse(raw: &str) -> Option<SourceUrl> {
    let url = url::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let parts = Parts::new(&url)?;
    PARSERS.iter().find_map(|parse| parse(&parts))
}

/// The site `raw` is on, if we know it.
pub fn site_of(raw: &str) -> Option<&'static Site> {
    parse(raw).map(|u| u.site)
}

/// `raw` as an artist's URL should be kept: a profile's canonical form
/// (`artstation.com/artist/x` becomes `https://www.artstation.com/x`),
/// anything else as it is.
pub fn canonical_artist_url(raw: &str) -> String {
    match parse(raw) {
        Some(found) if found.kind() == Kind::Profile => found.profile_url.unwrap_or_default(),
        _ => raw.to_owned(),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// `raw` parsed, which must be on `site`.
    pub fn on(site: &'static Site, raw: &str) -> SourceUrl {
        let found = parse(raw).unwrap_or_else(|| panic!("{raw} not recognised"));
        assert_eq!(found.site.key, site.key, "{raw}");
        found
    }

    /// Asserts `raw` is a work's page whose canonical form is `page`.
    pub fn page(site: &'static Site, raw: &str, page: &str) {
        let found = on(site, raw);
        assert_eq!(found.kind(), Kind::Page, "{raw}");
        assert_eq!(found.page_url.as_deref(), Some(page), "{raw}");
    }

    /// Asserts `raw` is a profile whose canonical form is `profile`.
    pub fn profile(site: &'static Site, raw: &str, profile: &str) {
        let found = on(site, raw);
        assert_eq!(found.kind(), Kind::Profile, "{raw}");
        assert_eq!(found.profile_url.as_deref(), Some(profile), "{raw}");
    }

    /// Asserts `raw` is a file whose full version is `full` (when known).
    pub fn file(site: &'static Site, raw: &str, full: Option<&str>) {
        let found = on(site, raw);
        assert_eq!(found.kind(), Kind::File, "{raw}");
        assert_eq!(found.file_url.as_deref(), full, "{raw}");
    }

    /// Asserts whether file `raw` is known to be a sample.
    pub fn sample(site: &'static Site, raw: &str, is_sample: bool) {
        let found = on(site, raw);
        assert_eq!(found.kind(), Kind::File, "{raw}");
        assert_eq!(found.is_sample, is_sample, "{raw}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_odd_links() {
        assert_eq!(parse("https://example.com/a"), None);
        assert_eq!(parse("javascript:alert(1)"), None);
        assert_eq!(parse("not a url"), None);
        // Any link on a known site is at least that site's.
        let other = parse("https://www.artstation.com/about").unwrap();
        assert_eq!(other.site.key, "artstation");
        assert_eq!(other.kind(), Kind::Other);
    }

    #[test]
    fn artist_urls_become_canonical() {
        assert_eq!(
            canonical_artist_url("https://artstation.com/artist/sa-dui"),
            "https://www.artstation.com/sa-dui"
        );
        // Works and unknown sites stay as they are.
        assert_eq!(
            canonical_artist_url("https://www.artstation.com/artwork/04XA4"),
            "https://www.artstation.com/artwork/04XA4"
        );
        assert_eq!(
            canonical_artist_url("https://example.com/me"),
            "https://example.com/me"
        );
    }

    #[test]
    fn site_keys_are_unique() {
        let mut keys: Vec<&str> = ALL.iter().map(|s| s.key).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count);
    }
}
