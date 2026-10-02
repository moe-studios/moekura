//! Itaku: images, posts and commissions (`itaku.ee/images/<id>`), profiles,
//! and gallery files (`…/<name>/xl.jpg` samples of `…/<name>.<ext>`).

use super::parts::Parts;
use super::{ITAKU, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "itaku.ee" {
        return None;
    }
    let found = SourceUrl::of(&ITAKU);
    let base = "https://itaku.ee";
    Some(match p.path().as_slice() {
        ["api", media, gallery, _, _]
            if media.starts_with("media") && gallery.contains("gallery") =>
        {
            found.file(None)
        }
        ["api", media, gallery, _] if media.starts_with("media") && gallery.contains("gallery") => {
            found.file(p.without_query())
        }
        ["api", media, ..] if media.starts_with("media") => found.file(None),
        ["images", id] | ["api", "galleries", "images", id, ..] => {
            found.page(format!("{base}/images/{id}"))
        }
        ["posts", id] | ["api", "posts", id, ..] => found.page(format!("{base}/posts/{id}")),
        ["commissions", id] | ["api", "commissions", id, ..] => {
            found.page(format!("{base}/commissions/{id}"))
        }
        ["profile", name, ..] if !matches!(*name, "about" | "help" | "home" | "tags") => {
            found.profile(format!("{base}/profile/{name}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn images_profiles_and_files() {
        page(
            &ITAKU,
            "https://itaku.ee/api/galleries/images/812661/comments/",
            "https://itaku.ee/images/812661",
        );
        page(
            &ITAKU,
            "https://itaku.ee/posts/130073",
            "https://itaku.ee/posts/130073",
        );
        profile(
            &ITAKU,
            "https://itaku.ee/profile/advosart/gallery",
            "https://itaku.ee/profile/advosart",
        );
        file(
            &ITAKU,
            "https://itaku.ee/api/media/gallery_imgs/IMG_2679_3GtFUgB/xl.jpg",
            None,
        );
        file(
            &ITAKU,
            "https://itaku.ee/api/media/gallery_imgs/IMG_2679_3GtFUgB.png",
            Some("https://itaku.ee/api/media/gallery_imgs/IMG_2679_3GtFUgB.png"),
        );
    }
}
