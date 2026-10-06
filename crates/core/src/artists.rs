//! Artist entries: an artist tag's other names, group, and the places the
//! artist posts (their URLs), used to recognise the artist of a file from
//! where it came from.

use url::Url;

/// URLs an artist can have.
pub const MAX_URLS: usize = 100;
/// Characters in one URL.
pub const URL_MAX_LEN: usize = 2048;
/// Characters in a group name.
pub const GROUP_MAX_LEN: usize = 170;

/// One of an artist's URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistUrl {
    pub url: String,
    /// Inactive URLs (a deleted account, an old site) still identify the
    /// artist but aren't linked as current.
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("An artist can have at most {MAX_URLS} URLs.")]
    TooMany,
    #[error("`{0}` isn't an http(s) address.")]
    NotWeb(String),
    #[error("URLs can be at most {URL_MAX_LEN} characters long.")]
    TooLong,
}

impl ArtistUrl {
    /// URLs from a form's box, one per line or separated by spaces; a `-`
    /// in front marks one inactive, as on Danbooru. Repeats are dropped
    /// (by their [`normalize_url`] form, the first staying).
    pub fn parse_list(input: &str) -> Result<Vec<Self>, UrlError> {
        let mut out: Vec<Self> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for word in input.split_whitespace() {
            let (raw, is_active) = match word.strip_prefix('-') {
                Some(rest) => (rest, false),
                None => (word, true),
            };
            if raw.is_empty() {
                continue;
            }
            if raw.len() > URL_MAX_LEN {
                return Err(UrlError::TooLong);
            }
            // Addresses typed without a scheme are common: `twitter.com/x`.
            let with_scheme = if raw.contains("://") {
                raw.to_owned()
            } else {
                format!("https://{raw}")
            };
            let url = Url::parse(&with_scheme)
                .ok()
                .filter(|u| matches!(u.scheme(), "http" | "https") && u.has_host())
                .ok_or_else(|| UrlError::NotWeb(raw.to_owned()))?;
            let normalized = normalize_url(url.as_str()).unwrap_or_default();
            if !seen.insert(normalized) {
                continue;
            }
            // A profile on a site we know is kept in its canonical form,
            // which encoding can make longer than what was typed (`'`
            // becomes `%27`), so the limit applies to it too.
            let url = crate::sites::canonical_artist_url(url.as_str());
            if url.len() > URL_MAX_LEN {
                return Err(UrlError::TooLong);
            }
            out.push(Self { url, is_active });
        }
        if out.len() > MAX_URLS {
            return Err(UrlError::TooMany);
        }
        Ok(out)
    }

    /// The URLs as [`Self::parse_list`] reads them, one per line.
    pub fn to_list(urls: &[Self]) -> String {
        urls.iter()
            .map(|u| {
                if u.is_active {
                    u.url.clone()
                } else {
                    format!("-{}", u.url)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A URL reduced for comparing: a profile on a site we know in its
/// canonical form ([`crate::sites`]), then no scheme, `www.` or
/// `mobile.`, query or fragment, lowercase host, and no trailing slash,
/// like `twitter.com/Artist`. `None` for anything but a web address.
pub fn normalize_url(raw: &str) -> Option<String> {
    let with_scheme = if raw.contains("://") {
        raw.trim().to_owned()
    } else {
        format!("https://{}", raw.trim())
    };
    let url = Url::parse(&crate::sites::canonical_artist_url(&with_scheme)).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("mobile."))
        .or_else(|| host.strip_prefix("m."))
        .unwrap_or(&host);
    // Twitter and X are one site.
    let host = if host == "x.com" { "twitter.com" } else { host };
    let path = url.path().trim_end_matches('/');
    Some(format!("{host}{path}"))
}

/// What an artist URL must equal to match `raw` (see [`normalize_url`]):
/// the URL itself and every shorter path on the same site, longest first,
/// but not the bare site, then the profile the URL belongs to on a site
/// we know. A post's URL, `pixiv.net/users/1/artworks`, then finds the
/// artist whose URL is `pixiv.net/users/1`, and
/// `artist.deviantart.com/art/x-1` the one with `deviantart.com/artist`.
pub fn url_prefixes(raw: &str) -> Vec<String> {
    let Some(normalized) = normalize_url(raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut current = normalized.as_str();
    while let Some((shorter, _)) = current.rsplit_once('/') {
        out.push(current.to_owned());
        current = shorter;
    }
    let with_scheme = if raw.contains("://") {
        raw.trim().to_owned()
    } else {
        format!("https://{}", raw.trim())
    };
    let profile = crate::sites::parse(&with_scheme)
        .and_then(|u| u.profile_url)
        .and_then(|p| normalize_url(&p));
    if let Some(profile) = profile
        && !out.contains(&profile)
    {
        out.push(profile);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_url("https://www.Pixiv.net/users/123/?lang=en#top").as_deref(),
            Some("pixiv.net/users/123")
        );
        assert_eq!(
            normalize_url("x.com/Artist").as_deref(),
            Some("twitter.com/Artist")
        );
        assert_eq!(normalize_url("ftp://example.com/x"), None);
        // Profiles on known sites compare in their canonical form.
        assert_eq!(
            normalize_url("https://www.artstation.com/artist/sa-dui/albums").as_deref(),
            Some("artstation.com/sa-dui")
        );
        assert_eq!(
            normalize_url("https://www.pixiv.net/member.php?id=5").as_deref(),
            Some("pixiv.net/users/5")
        );
    }

    #[test]
    fn prefixes_stop_before_the_site() {
        assert_eq!(
            url_prefixes("https://twitter.com/artist/status/1"),
            [
                "twitter.com/artist/status/1",
                "twitter.com/artist/status",
                "twitter.com/artist"
            ]
        );
        assert!(url_prefixes("https://twitter.com/").is_empty());
        // A work's page also finds its artist's profile.
        assert_eq!(
            url_prefixes("https://noizave.deviantart.com/art/test-685436408")
                .last()
                .map(String::as_str),
            Some("deviantart.com/noizave")
        );
    }

    #[test]
    fn parses_url_lists() {
        let urls = ArtistUrl::parse_list(
            "https://twitter.com/a\n-pixiv.net/users/1  https://www.twitter.com/a/",
        )
        .unwrap();
        assert_eq!(
            urls,
            [
                ArtistUrl {
                    url: "https://x.com/a".into(),
                    is_active: true
                },
                ArtistUrl {
                    url: "https://www.pixiv.net/users/1".into(),
                    is_active: false
                },
            ]
        );
        assert_eq!(
            ArtistUrl::parse_list(&ArtistUrl::to_list(&urls)).unwrap(),
            urls
        );
        assert!(matches!(
            ArtistUrl::parse_list("javascript:alert(1)"),
            Err(UrlError::NotWeb(_))
        ));
    }

    #[test]
    fn long_url_lists() {
        let urls = |n: usize, m: usize| -> String {
            (0..n)
                .map(|i| format!("https://a.example/{} ", i % m))
                .collect()
        };
        assert_eq!(
            ArtistUrl::parse_list(&urls(50_000, MAX_URLS))
                .unwrap()
                .len(),
            MAX_URLS
        );
        assert_eq!(
            ArtistUrl::parse_list(&urls(50_000, 50_000)),
            Err(UrlError::TooMany)
        );
    }

    #[test]
    fn the_kept_form_is_held_to_the_limit() {
        // Short enough as typed, three times as long once encoded.
        let quotes = "'".repeat(1000);
        assert_eq!(
            ArtistUrl::parse_list(&format!("https://example.com/{quotes}")),
            Err(UrlError::TooLong)
        );
        let urls =
            ArtistUrl::parse_list(&format!("https://example.com/{}", &quotes[..600])).unwrap();
        assert_eq!(urls[0].url.len(), "https://example.com/".len() + 3 * 600);
    }
}
