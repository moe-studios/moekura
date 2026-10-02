//! A URL taken apart the way the site parsers match on it: host, the
//! registrable domain and what comes before it, decoded path segments,
//! query parameters, and the file name.

use url::Url;

/// Second-level suffixes under which a domain has three labels
/// (`example.co.jp`), for the countries the sites we know are in.
const TWO_LABEL_SUFFIXES: &[&str] = &[
    "co.jp", "ne.jp", "or.jp", "ac.jp", "com.cn", "net.cn", "co.kr", "or.kr", "com.tw", "co.uk",
    "com.au", "com.br", "com.hk", "com.es",
];

/// File extensions of images, videos and the like.
const FILE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "jxl", "bmp", "webm", "mp4", "mov", "m4v", "swf",
    "zip", "mp3", "ogg", "wav", "flac",
];

pub(crate) struct Parts<'a> {
    pub url: &'a Url,
    /// Lowercase.
    pub host: String,
    /// The registrable domain: `pixiv.net` for `i.pximg.net`'s
    /// `pximg.net`, `infoseek.co.jp` for `a.b.infoseek.co.jp`.
    pub domain: String,
    /// What comes before the domain (`www`, `a.b`), or empty.
    pub sub: String,
    /// The path's non-empty segments, percent-decoded.
    pub segs: Vec<String>,
}

impl<'a> Parts<'a> {
    pub fn new(url: &'a Url) -> Option<Self> {
        let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
        let labels: Vec<&str> = host.split('.').collect();
        let keep = if labels.len() >= 3
            && TWO_LABEL_SUFFIXES.contains(&labels[labels.len() - 2..].join(".").as_str())
        {
            3
        } else {
            2
        };
        let split = labels.len().saturating_sub(keep);
        let domain = labels[split..].join(".");
        let sub = labels[..split].join(".");
        let segs = url
            .path_segments()
            .map(|segments| {
                segments
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        percent_encoding::percent_decode_str(s)
                            .decode_utf8_lossy()
                            .into_owned()
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            url,
            host,
            domain,
            sub,
            segs,
        })
    }

    /// The segments, for matching on as a slice.
    pub fn path(&self) -> Vec<&str> {
        self.segs.iter().map(String::as_str).collect()
    }

    /// A query parameter, decoded.
    pub fn param(&self, name: &str) -> Option<String> {
        self.url
            .query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
            .filter(|v| !v.is_empty())
    }

    pub fn query(&self) -> Option<&str> {
        self.url.query().filter(|q| !q.is_empty())
    }

    pub fn fragment(&self) -> Option<&str> {
        self.url.fragment().filter(|f| !f.is_empty())
    }

    /// The last segment, extension and all.
    pub fn basename(&self) -> Option<&str> {
        self.segs.last().map(String::as_str)
    }

    /// The last segment without its extension.
    pub fn stem(&self) -> Option<&str> {
        let base = self.basename()?;
        Some(base.rsplit_once('.').map_or(base, |(stem, _)| stem))
    }

    /// The last segment's extension, lowercase.
    pub fn ext(&self) -> Option<String> {
        let (_, ext) = self.basename()?.rsplit_once('.')?;
        (!ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()))
            .then(|| ext.to_ascii_lowercase())
    }

    /// Whether the path ends in an image's or video's extension.
    pub fn has_file_ext(&self) -> bool {
        self.ext()
            .is_some_and(|ext| FILE_EXTENSIONS.contains(&ext.as_str()))
    }

    /// `https://<host>`.
    pub fn origin(&self) -> String {
        format!("https://{}", self.host)
    }

    /// The URL as given.
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    /// The URL without its query and fragment, on https.
    pub fn without_query(&self) -> String {
        let mut url = self.url.clone();
        url.set_query(None);
        url.set_fragment(None);
        let _ = url.set_scheme("https");
        url.to_string()
    }
}

/// Whether `s` is `len` hexadecimal digits.
pub(crate) fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether `s` is a non-empty run of ASCII digits.
pub(crate) fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// The leading digits of `s`, if it starts with any.
pub(crate) fn leading_digits(s: &str) -> Option<&str> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    (end > 0).then(|| &s[..end])
}

/// Whether `s` looks like a UUID (`8-4-4-4-12` hex digits).
pub(crate) fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, len)| is_hex(g, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_urls_apart() {
        let url = Url::parse("https://A.B.Example.co.jp/x/%E7%8C%AB/file.PNG?q=1#f").unwrap();
        let p = Parts::new(&url).unwrap();
        assert_eq!(p.host, "a.b.example.co.jp");
        assert_eq!(p.domain, "example.co.jp");
        assert_eq!(p.sub, "a.b");
        assert_eq!(p.path(), ["x", "猫", "file.PNG"]);
        assert_eq!(p.stem(), Some("file"));
        assert_eq!(p.ext().as_deref(), Some("png"));
        assert!(p.has_file_ext());
        assert_eq!(p.param("q").as_deref(), Some("1"));
        assert_eq!(p.fragment(), Some("f"));
        let url = Url::parse("https://pixiv.net/").unwrap();
        let p = Parts::new(&url).unwrap();
        assert_eq!((p.domain.as_str(), p.sub.as_str()), ("pixiv.net", ""));
        assert!(p.path().is_empty());
        assert!(is_uuid("8bb9e4e3-d171-4027-88df-84480480f79d"));
        assert_eq!(leading_digits("123abc"), Some("123"));
    }
}
