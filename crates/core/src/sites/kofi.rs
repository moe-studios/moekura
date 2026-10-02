//! Ko-fi: gallery items (`ko-fi.com/i/<id>`), shop items, commissions,
//! posts, albums, pages, and uploads.

use super::parts::Parts;
use super::{KOFI, SourceUrl};

const RESERVED: &[&str] = &[
    "c",
    "i",
    "s",
    "about",
    "account",
    "album",
    "cdn",
    "commissions",
    "discord",
    "explore",
    "gallery",
    "gold",
    "memberships",
    "post",
    "privacy",
    "shop",
    "terms",
    "manage",
    "home",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let cdn = matches!(
        p.host.as_str(),
        "az743702.vo.msecnd.net" | "ko-fi-live.azurewebsites.net"
    );
    if p.domain != "ko-fi.com" && !cdn {
        return None;
    }
    let found = SourceUrl::of(&KOFI);
    let base = "https://ko-fi.com";
    let path = p.path();
    if cdn || path.first() == Some(&"cdn") {
        return Some(match path.as_slice() {
            ["cdn", "useruploads", "post" | "display", file] => found.file(format!(
                "https://storage.ko-fi.com/cdn/useruploads/display/{file}"
            )),
            _ => found.file(None),
        });
    }
    let item = p.param("viewimage");
    Some(match path.as_slice() {
        ["i", id] => found.page(format!("{base}/i/{id}")),
        ["s", id] => found.page(format!("{base}/s/{id}")),
        ["c", id] => found.page(format!("{base}/c/{id}")),
        ["post", slug] => found.page(format!("{base}/post/{slug}")),
        ["album", slug] => found.page(format!("{base}/album/{slug}")),
        ["Gallery", "LockedGalleryItem"] => match p.param("id") {
            Some(id) => found.page(format!("{base}/i/{id}")),
            None => found,
        },
        [name, ..] if !RESERVED.contains(&name.to_ascii_lowercase().as_str()) => {
            let profile = format!("{base}/{name}");
            match item {
                Some(id) => found
                    .page(format!("{profile}?viewimage={id}"))
                    .profile(profile),
                None => found.profile(profile),
            }
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn items_pages_and_uploads() {
        page(
            &KOFI,
            "https://ko-fi.com/i/IU7U5SS0YZ",
            "https://ko-fi.com/i/IU7U5SS0YZ",
        );
        page(
            &KOFI,
            "https://ko-fi.com/thom_sketching/gallery?viewimage=IO5O1BOYV6#galleryItemView",
            "https://ko-fi.com/thom_sketching?viewimage=IO5O1BOYV6",
        );
        profile(
            &KOFI,
            "https://ko-fi.com/johndaivid",
            "https://ko-fi.com/johndaivid",
        );
        file(
            &KOFI,
            "https://storage.ko-fi.com/cdn/useruploads/post/2c42fc4c-6ebb-4b09-9da0-d14b19a105b1_d69ed1ef.png",
            Some(
                "https://storage.ko-fi.com/cdn/useruploads/display/2c42fc4c-6ebb-4b09-9da0-d14b19a105b1_d69ed1ef.png",
            ),
        );
    }
}
