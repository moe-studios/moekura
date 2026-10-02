//! Bilibili: dynamics and opus posts, illustrations (`h.bilibili.com`),
//! articles, videos, manga, users (`space.bilibili.com/<id>`), and images
//! on hdslb.com (`…/x.jpg@1036w.webp` samples of `…/x.jpg`).

use super::parts::{Parts, is_digits};
use super::{BILIBILI, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "bilibili.com" | "biliimg.com" | "hdslb.com" | "bili2233.cn" | "b23.tv" | "bilibili.tv"
    ) || p.host == "live.bilibili.com"
    {
        return None;
    }
    let found = SourceUrl::of(&BILIBILI);
    let path = p.path();
    if matches!(p.domain.as_str(), "hdslb.com" | "biliimg.com") {
        let full = p.without_query();
        // Drop the `@…` sample suffix.
        let full = match full.rsplit_once('@') {
            Some((original, _)) if original.rsplit('/').next().is_some_and(|f| f.contains('.')) => {
                original.to_owned()
            }
            _ => full,
        };
        // new_dyn files end with the artist's id: `<32 chars><uid>.jpg`.
        let artist = match path.as_slice() {
            ["bfs", "new_dyn", file] => file
                .split('.')
                .next()
                .and_then(|stem| stem.get(32..))
                .filter(|id| id.len() >= 8 && is_digits(id))
                .map(|id| format!("https://space.bilibili.com/{id}")),
            _ => None,
        };
        return Some(found.file(full).profile(artist));
    }
    let www = matches!(p.sub.as_str(), "" | "www" | "m");
    let opus = |id: &str| format!("https://www.bilibili.com/opus/{id}");
    Some(match (p.sub.as_str(), path.as_slice()) {
        ("t", [id]) if is_digits(id) => found.page(opus(id)),
        ("m", ["dynamic", id]) if is_digits(id) => found.page(opus(id)),
        (_, ["opus", id]) if www && is_digits(id) => found.page(opus(id)),
        ("h", [id]) if is_digits(id) => found.page(format!("https://h.bilibili.com/{id}")),
        (_, ["p", "h5", id]) if www && is_digits(id) => {
            found.page(format!("https://h.bilibili.com/{id}"))
        }
        (_, ["read", cv]) if www && cv.starts_with("cv") => {
            found.page(format!("https://www.bilibili.com/read/{cv}/"))
        }
        ("space", [id, ..]) | ("m", ["space", id, ..]) if is_digits(id) => {
            found.profile(format!("https://space.bilibili.com/{id}"))
        }
        (_, ["video", id] | ["s", "video", id]) if www => {
            found.page(format!("https://www.bilibili.com/video/{id}/"))
        }
        ("manga", ["detail", id]) if id.starts_with("mc") => {
            found.page(format!("https://manga.bilibili.com/detail/{id}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, on, page, profile};
    use super::*;

    #[test]
    fn posts_users_and_images() {
        page(
            &BILIBILI,
            "https://t.bilibili.com/686082748803186697",
            "https://www.bilibili.com/opus/686082748803186697",
        );
        page(
            &BILIBILI,
            "https://h.bilibili.com/83341894",
            "https://h.bilibili.com/83341894",
        );
        page(
            &BILIBILI,
            "https://www.bilibili.com/video/BV1dY4y1u7Vi/",
            "https://www.bilibili.com/video/BV1dY4y1u7Vi/",
        );
        profile(
            &BILIBILI,
            "https://space.bilibili.com/476725595/dynamic",
            "https://space.bilibili.com/476725595",
        );
        file(
            &BILIBILI,
            "https://i0.hdslb.com/bfs/album/37f77871d417c76a08a9467527e9670810c4c442.gif@1036w.webp",
            Some("https://i0.hdslb.com/bfs/album/37f77871d417c76a08a9467527e9670810c4c442.gif"),
        );
        let dynamic = on(
            &BILIBILI,
            "https://i0.hdslb.com/bfs/new_dyn/675526fd8baa2f75d7ea0e7ea957bc0811742550.jpg@1036w.webp",
        );
        assert_eq!(
            dynamic.profile_url.as_deref(),
            Some("https://space.bilibili.com/11742550")
        );
    }
}
