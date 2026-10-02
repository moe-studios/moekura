//! Pixiv: works (`/artworks/<id>`, `member_illust.php`, `/i/<id>`),
//! novels, users (`/users/<id>`, `/stacc/<name>`, `pixiv.me/<name>`), and
//! files on `i.pximg.net`.

use super::parts::{Parts, is_digits, is_hex};
use super::{PIXIV, SourceUrl};

const DOMAINS: &[&str] = &[
    "pximg.net",
    "pixiv.net",
    "pixiv.me",
    "pixiv.cc",
    "p.tl",
    "phixiv.net",
];

/// Image types with a date path (`/img-original/img/2014/10/03/18/10/20/…`).
const DATED_TYPES: &[&str] = &[
    "img-original",
    "img-master",
    "img-zip-ugoira",
    "img-inf",
    "custom-thumb",
    "novel-cover-original",
    "novel-cover-master",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !DOMAINS.contains(&p.domain.as_str()) {
        return None;
    }
    let found = SourceUrl::of(&PIXIV);
    let is_image = matches!(p.host.as_str(), "i.pximg.net" | "i-f.pximg.net")
        || (p.domain == "pixiv.net"
            && ["i", "img"]
                .iter()
                .any(|prefix| p.sub.strip_prefix(prefix).is_some_and(is_digits)));
    let path = p.path();
    if is_image {
        return Some(image(p, &path, found));
    }
    let work = |id: &str| format!("https://www.pixiv.net/artworks/{id}");
    let user = |id: &str| format!("https://www.pixiv.net/users/{id}");
    let stacc = |name: &str| format!("https://www.pixiv.net/stacc/{name}");
    let pixiv = matches!(p.domain.as_str(), "pixiv.net" | "phixiv.net");
    Some(match (p.domain.as_str(), path.as_slice()) {
        (_, [.., "artworks", id]) if pixiv && is_digits(id) => found.page(work(id)),
        ("pixiv.net", ["novel", "show.php"]) | ("pixiv.net", ["novel.php"]) => {
            match p.param("id") {
                Some(id) => found.page(format!("https://www.pixiv.net/novel/show.php?id={id}")),
                None => found,
            }
        }
        ("pixiv.net", ["novel", "series", id]) if is_digits(id) => {
            found.page(format!("https://www.pixiv.net/novel/series/{id}"))
        }
        ("pixiv.net", ["i", id]) | ("p.tl", ["i", id]) if is_digits(id) => found.page(work(id)),
        (_, ["member_illust.php"]) if pixiv => match p.param("illust_id") {
            Some(id) if is_digits(&id) => found.page(work(&id)),
            _ => found,
        },
        ("pixiv.net", [.., "member.php"]) => match p.param("id") {
            Some(id) if is_digits(&id) => found.profile(user(&id)),
            _ => found,
        },
        ("pixiv.net", ["u" | "user" | "users", id, ..])
        | ("pixiv.net", [_, "u" | "users", id, ..])
        | ("p.tl", ["m", id])
            if is_digits(id) =>
        {
            found.profile(user(id))
        }
        ("pixiv.net", ["stacc", name]) | ("pixiv.me" | "pixiv.cc", [name]) => {
            found.profile(stacc(name))
        }
        _ => found,
    })
}

/// A file on Pixiv's image servers: its work, and the original when the
/// link is a sample that names it.
fn image(p: &Parts, path: &[&str], found: SourceUrl) -> SourceUrl {
    let Some(stem) = p.stem() else {
        return found.file(None);
    };
    let pieces: Vec<&str> = stem.split('_').collect();
    // `<id>` or `<id>-<hash>`, then `p<page>` or `ugoira…`.
    let (id, hash) = match pieces.first().map(|first| first.split_once('-')) {
        Some(Some((id, hash))) if is_digits(id) && is_hex(hash, 32) => (Some(id), Some(hash)),
        Some(None) if is_digits(pieces[0]) => (Some(pieces[0]), None),
        _ => (None, None),
    };
    let page = pieces
        .get(1)
        .and_then(|p| p.strip_prefix('p'))
        .filter(|n| is_digits(n));
    let dated = path
        .iter()
        .position(|s| DATED_TYPES.contains(s))
        .filter(|&at| path.get(at + 1) == Some(&"img") && path.len() == at + 9);
    let full = dated.and_then(|at| {
        let kind = path[at];
        let date = path[at + 2..at + 8].join("/");
        let ext = p.ext()?;
        match kind {
            "img-original" | "novel-cover-original" => Some(format!(
                "https://i.pximg.net/{kind}/img/{date}/{}",
                p.basename()?
            )),
            // A sample's original, if its extension is the same (it often
            // isn't: the strategy reads the real one from the API).
            "img-master" | "custom-thumb" => {
                let id = id?;
                let page = page?;
                let id = match hash {
                    Some(hash) => format!("{id}-{hash}"),
                    None => id.to_owned(),
                };
                Some(format!(
                    "https://i.pximg.net/img-original/img/{date}/{id}_p{page}.{ext}"
                ))
            }
            _ => None,
        }
    });
    // Anything but an original (or a ugoira's zip) is a resized copy.
    let original = dated.is_some_and(|at| {
        matches!(
            path[at],
            "img-original" | "novel-cover-original" | "img-zip-ugoira"
        )
    });
    let found = found.file(full).sample(dated.is_some() && !original);
    match id {
        Some(id) if !stem.starts_with("ci") && !stem.starts_with("sci") => {
            found.page(format!("https://www.pixiv.net/artworks/{id}"))
        }
        _ => found,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, on, page, profile, sample};
    use super::*;

    #[test]
    fn works_and_files() {
        page(
            &PIXIV,
            "https://www.pixiv.net/en/artworks/46324488",
            "https://www.pixiv.net/artworks/46324488",
        );
        page(
            &PIXIV,
            "http://www.pixiv.net/member_illust.php?mode=medium&illust_id=18557054",
            "https://www.pixiv.net/artworks/18557054",
        );
        page(
            &PIXIV,
            "http://p.tl/i/40009777",
            "https://www.pixiv.net/artworks/40009777",
        );
        page(
            &PIXIV,
            "https://www.pixiv.net/novel/show.php?id=9434677&mode=cover",
            "https://www.pixiv.net/novel/show.php?id=9434677",
        );
        file(
            &PIXIV,
            "https://i.pximg.net/img-original/img/2014/10/03/18/10/20/46324488_p0.png",
            Some("https://i.pximg.net/img-original/img/2014/10/03/18/10/20/46324488_p0.png"),
        );
        file(
            &PIXIV,
            "https://i.pximg.net/c/250x250_80_a2/img-master/img/2014/10/29/09/27/19/46785915_p0_square1200.jpg",
            Some("https://i.pximg.net/img-original/img/2014/10/29/09/27/19/46785915_p0.jpg"),
        );
        let found = on(
            &PIXIV,
            "https://i.pximg.net/img-master/img/2014/10/03/18/10/20/46324488_p0_master1200.jpg",
        );
        assert_eq!(
            found.page_url.as_deref(),
            Some("https://www.pixiv.net/artworks/46324488")
        );
        file(
            &PIXIV,
            "http://i2.pixiv.net/img18/img/evazion/14901720.png",
            None,
        );
    }

    #[test]
    fn samples() {
        for (raw, is_sample) in [
            (
                "https://i.pximg.net/img-original/img/2014/10/03/18/10/20/46324488_p0.png",
                false,
            ),
            (
                "https://i.pximg.net/img-master/img/2014/10/03/18/10/20/46324488_p0_master1200.jpg",
                true,
            ),
            (
                "https://i.pximg.net/c/250x250_80_a2/img-master/img/2014/10/29/09/27/19/46785915_p0_square1200.jpg",
                true,
            ),
            (
                "https://i.pximg.net/img-zip-ugoira/img/2016/04/09/14/25/29/56268141_ugoira1920x1080.zip",
                false,
            ),
        ] {
            sample(&PIXIV, raw, is_sample);
        }
    }

    #[test]
    fn profiles() {
        profile(
            &PIXIV,
            "https://www.pixiv.net/member.php?id=339253",
            "https://www.pixiv.net/users/339253",
        );
        profile(
            &PIXIV,
            "https://www.pixiv.net/en/users/76567/novels",
            "https://www.pixiv.net/users/76567",
        );
        profile(
            &PIXIV,
            "http://www.pixiv.me/noizave",
            "https://www.pixiv.net/stacc/noizave",
        );
        profile(
            &PIXIV,
            "https://www.pixiv.net/stacc/noizave",
            "https://www.pixiv.net/stacc/noizave",
        );
    }
}
