//! Links on the sites [`moekura_core::sites`] recognises: the site's
//! strategy reads the work's page; a site without one (or a link that
//! isn't a work) gets what the link itself says.

use moekura_core::sites::{Kind, SourceUrl, encoded_url};
use url::Url;

use super::{
    Http, SourceInfo, apple_music, arca_live, art_station, art_street, artistree, behance,
    bilibili, blogger, boorus, booth, carrd, ci_en, dc_inside, dotpict, fandom, fantia, fc2,
    fediverse, foriio, four_chan, furaffinity, galleria, grafolio, gumroad, hentai_foundry, huajia,
    huashijie, imgur, inkbunny, itaku, kofi, lofter, mihuashi, minitokyo, miyoushe, my_portfolio,
    naver, newgrounds, nico_seiga, nijie, note, odaibako, opengraph, opensea, patreon, piapro,
    pinterest, pixiv_family, plurk, poipiku, postype, privatter, reddit, redgifs, tinami, tistory,
    toyhouse, tumblr, vk, weibo, xfolio, xiaohongshu, yachiyo_room, youtube,
};

/// The sites whose works a strategy reads (here, or in [`super`] for the
/// first few sites), by key: the bookmarklet page lists them.
pub(crate) const READ: &[&str] = &[
    "adobe_portfolio",
    "apple_music",
    "arca_live",
    "artistree",
    "artstation",
    "artstreet",
    "baraag",
    "behance",
    "bilibili",
    "blogger",
    "bluesky",
    "booth",
    "carrd",
    "ci_en",
    "danbooru",
    "dc_inside",
    "deviantart",
    "dotpict",
    "e621",
    "fanbox",
    "fandom",
    "fantia",
    "fc2",
    "foriio",
    "fourchan",
    "furaffinity",
    "galleria",
    "gelbooru",
    "grafolio",
    "gumroad",
    "hentai_foundry",
    "huajia",
    "huashijie",
    "imgur",
    "inkbunny",
    "itaku",
    "kofi",
    "konachan",
    "lofter",
    "mihuashi",
    "minitokyo",
    "misskey",
    "misskey_art",
    "misskey_design",
    "misskey_io",
    "miyoushe",
    "naver_blog",
    "naver_cafe",
    "newgrounds",
    "nico_seiga",
    "nijie",
    "note",
    "odaibako",
    "opensea",
    "patreon",
    "pawoo",
    "piapro",
    "pinterest",
    "pixiv",
    "pixiv_comic",
    "pixiv_factory",
    "pixiv_sketch",
    "plurk",
    "poipiku",
    "postype",
    "privatter",
    "reddit",
    "redgifs",
    "rule34_us",
    "rule34_xxx",
    "safebooru",
    "skeb",
    "tbib",
    "tinami",
    "tistory",
    "toyhouse",
    "tumblr",
    "twitter",
    "vk",
    "weibo",
    "xfolio",
    "xiaohongshu",
    "yachiyo_room",
    "yandere",
    "youtube",
    "zerochan",
];

/// What `url` (on a known site, as `known`) says.
pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    url: &Url,
    depth: u8,
) -> Result<Option<SourceInfo>, String> {
    if depth < 2 && is_short_link(url) {
        // Where it leads is what it says.
        let target = http.final_url(url.as_str(), &[]).await?;
        if target.host_str() == url.host_str() {
            return Ok(None);
        }
        return http.redirected(&target, depth + 1).await;
    }
    let read = match known.page_url.as_deref() {
        Some(page) => strategy(http, known, page, depth).await,
        None => None,
    };
    let read = match read {
        Some(Ok(info)) => Some(info),
        // A site that changed (or wants a login) still has a page.
        Some(Err(error)) => {
            tracing::info!(url = %url, error, "the site's strategy failed; reading the page");
            None
        }
        None => None,
    };
    let mut info = match read {
        Some(info) => info,
        None => match generic(http, known, url).await? {
            Some(info) => info,
            None => return Ok(None),
        },
    };
    if known.kind() == Kind::File {
        // The file linked to comes first (its original, when known).
        let wanted = known.file_url.clone().unwrap_or_else(|| url.to_string());
        info.files.retain(|f| *f != wanted);
        info.files.insert(0, wanted);
    }
    if let Some(profile) = &known.profile_url
        && !info.profile_urls.contains(profile)
    {
        info.profile_urls.push(profile.clone());
    }
    Ok(Some(info))
}

/// Hosts whose links only redirect to a site's page.
const SHORT_LINK_HOSTS: &[&str] = &[
    "t.co",
    "pic.twitter.com",
    "pic.x.com",
    "b23.tv",
    "bili2233.cn",
    "pin.it",
    "tmblr.co",
    "posty.pe",
    "hoyo.link",
    "xhslink.com",
    "xhslink.cn",
    "vt.tiktok.com",
    "gum.co",
    "t.cn",
    "goo.gl",
    "photos.app.goo.gl",
    "vk.cc",
];

/// Whether `url` is a short link (or a site's redirect page) to follow.
fn is_short_link(url: &Url) -> bool {
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_start_matches("www.");
    let path = url.path();
    (SHORT_LINK_HOSTS.contains(&host) && path.len() > 1)
        || (host == "tiktok.com" && path.starts_with("/t/"))
}

/// The site's strategy for the work at `page`, if it has one.
async fn strategy(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
    depth: u8,
) -> Option<Result<SourceInfo, String>> {
    Some(match known.site.key {
        "danbooru" | "e621" | "gelbooru" | "safebooru" | "tbib" | "rule34_xxx" | "yandere"
        | "konachan" => boorus::fetch(http, known, page, depth).await,
        "rule34_us" => boorus::rule34_us(http, known, page).await,
        "zerochan" => boorus::zerochan(http, known, page).await,
        "pixiv_sketch" => pixiv_family::sketch(http, known, page).await,
        "pixiv_comic" => pixiv_family::comic(http, known, page).await,
        "pixiv_factory" => pixiv_family::factory(http, known, page).await,
        "misskey" | "misskey_io" | "misskey_art" | "misskey_design" => {
            fediverse::misskey(http, known.site, page).await
        }
        "pawoo" | "baraag" => fediverse::mastodon(http, known, page).await,
        "fourchan" => four_chan::fetch(http, known, page).await,
        "adobe_portfolio" => my_portfolio::fetch(http, known, page).await,
        "apple_music" => apple_music::fetch(http, known, page).await,
        "arca_live" => arca_live::fetch(http, known, page).await,
        "artistree" => artistree::fetch(http, known, page).await,
        "artstation" => art_station::fetch(http, known, page).await,
        "artstreet" => art_street::fetch(http, known, page).await,
        "behance" => behance::fetch(http, known, page).await,
        "bilibili" => bilibili::fetch(http, known, page).await,
        "blogger" => blogger::fetch(http, known, page).await,
        "booth" => booth::fetch(http, known, page).await,
        "carrd" => carrd::fetch(http, known, page).await,
        "ci_en" => ci_en::fetch(http, known, page).await,
        "dc_inside" => dc_inside::fetch(http, known, page).await,
        "dotpict" => dotpict::fetch(http, known, page).await,
        "fandom" => fandom::fetch(http, known, page).await,
        "fantia" => fantia::fetch(http, known, page).await,
        "fc2" => fc2::fetch(http, known, page).await,
        "foriio" => foriio::fetch(http, known, page).await,
        "furaffinity" => furaffinity::fetch(http, known, page).await,
        "galleria" => galleria::fetch(http, known, page).await,
        "grafolio" => grafolio::fetch(http, known, page).await,
        "gumroad" => gumroad::fetch(http, known, page).await,
        "hentai_foundry" => hentai_foundry::fetch(http, known, page).await,
        "imgur" => imgur::fetch(http, known, page).await,
        "inkbunny" => inkbunny::fetch(http, known, page).await,
        "itaku" => itaku::fetch(http, known, page).await,
        "kofi" => kofi::fetch(http, known, page).await,
        "lofter" => lofter::fetch(http, known, page).await,
        "mihuashi" => mihuashi::fetch(http, known, page).await,
        "minitokyo" => minitokyo::fetch(http, known, page).await,
        "miyoushe" => miyoushe::fetch(http, known, page).await,
        "naver_blog" => naver::blog(http, known, page).await,
        "naver_cafe" => naver::cafe(http, known, page).await,
        "newgrounds" => newgrounds::fetch(http, known, page).await,
        "nico_seiga" => nico_seiga::fetch(http, known, page).await,
        "nijie" => nijie::fetch(http, known, page).await,
        "note" => note::fetch(http, known, page).await,
        "odaibako" => odaibako::fetch(http, known, page).await,
        "opensea" => opensea::fetch(http, known, page).await,
        "patreon" => patreon::fetch(http, known, page).await,
        "piapro" => piapro::fetch(http, known, page).await,
        "pinterest" => pinterest::fetch(http, known, page).await,
        "plurk" => plurk::fetch(http, known, page).await,
        "poipiku" => poipiku::fetch(http, known, page).await,
        "postype" => postype::fetch(http, known, page).await,
        "privatter" => privatter::fetch(http, known, page).await,
        "reddit" => reddit::fetch(http, known, page).await,
        "redgifs" => redgifs::fetch(http, known, page).await,
        "tinami" => tinami::fetch(http, known, page).await,
        "tistory" => tistory::fetch(http, known, page).await,
        "toyhouse" => toyhouse::fetch(http, known, page).await,
        "tumblr" => tumblr::fetch(http, known, page).await,
        "vk" => vk::fetch(http, known, page).await,
        "weibo" => weibo::fetch(http, known, page).await,
        "xfolio" => xfolio::fetch(http, known, page).await,
        "xiaohongshu" => xiaohongshu::fetch(http, known, page).await,
        "yachiyo_room" => yachiyo_room::fetch(http, known, page).await,
        "youtube" => youtube::fetch(http, known, page).await,
        "huajia" => huajia::fetch(http, known, page).await,
        "huashijie" => huashijie::fetch(http, known, page).await,
        _ => return None,
    })
}

/// A known site's link without a strategy: a file is downloaded as it
/// is (its original, when known), a work's page is read for OpenGraph
/// tags, and anything else gives nothing.
async fn generic(
    http: &Http<'_>,
    known: &SourceUrl,
    url: &Url,
) -> Result<Option<SourceInfo>, String> {
    // Canonical forms come encoded (`sites::parse`); so must the link as
    // typed, which `Url` leaves with `'` in its path.
    let link = encoded_url(url.as_str());
    let page = known.page_url.clone();
    match known.kind() {
        Kind::File => {
            let mut info = SourceInfo::new(known.site, page.unwrap_or_else(|| link.clone()));
            info.files = vec![known.file_url.clone().unwrap_or(link)];
            Ok(Some(info))
        }
        Kind::Page => {
            let page = page.unwrap_or(link);
            let parsed = Url::parse(&page).map_err(|e| e.to_string())?;
            let found = opengraph::fetch(http, &parsed).await.unwrap_or(None);
            let mut info = found.unwrap_or_else(|| SourceInfo::new(known.site, page.clone()));
            info.site = known.site.name;
            info.page_url = page;
            // A page's preview image may be the site's logo (a page for
            // members, say): only a file on a site we know is the work.
            info.files
                .retain(|file| moekura_core::sites::parse(file).is_some_and(|u| u.is_file));
            Ok(Some(info))
        }
        Kind::Profile | Kind::Other => Ok(None),
    }
}

/// A note on a Misskey instance we don't know by name
/// (`<instance>/notes/<id>`), if the instance answers Misskey's API.
pub(super) async fn other_misskey(http: &Http<'_>, url: &Url) -> Option<SourceInfo> {
    fediverse::other_misskey(http, url).await
}

#[cfg(test)]
mod tests {
    use super::READ;

    #[test]
    fn read_sites_are_known_sites() {
        for key in READ {
            assert!(
                moekura_core::sites::ALL.iter().any(|s| s.key == *key),
                "{key}"
            );
        }
    }
}
