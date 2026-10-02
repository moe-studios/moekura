//! piapro: contents (`piapro.jp/t/<id>`, `/content/<id>`), creators, and
//! images.

use super::parts::Parts;
use super::{PIAPRO, SourceUrl};

const RESERVED: &[&str] = &[
    "3dm",
    "a",
    "about_us",
    "bookmark",
    "characters",
    "content",
    "content_list_recommend",
    "dm",
    "download",
    "faq",
    "follow",
    "help",
    "illust",
    "intro",
    "inquiry",
    "jump",
    "license",
    "logout",
    "mailto",
    "music",
    "my_page",
    "official_collabo",
    "privacypolicy",
    "product",
    "r",
    "t",
    "text",
    "timg",
    "user",
    "user_agreement",
    "user_mod",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "piapro.jp" {
        return None;
    }
    let found = SourceUrl::of(&PIAPRO);
    let content = |id: &str| format!("https://piapro.jp/content/{id}");
    let path = p.path();
    let content_of = |file: &str| file.split('_').next().map(str::to_owned);
    Some(match (p.sub.as_str(), path.as_slice()) {
        ("cdn", ["thumb_i", _, file]) => found
            .file(None)
            .page(content_of(file).map(|id| content(&id))),
        ("dl", ["image", _, file]) => found
            .file(p.as_str().to_owned())
            .page(content_of(file).map(|id| content(&id))),
        ("cdn" | "dl" | "blog", _) => found.file(None),
        (_, ["t", id, ..]) => found.page(format!("https://piapro.jp/t/{id}")),
        (_, ["content", id]) => found.page(content(id)),
        (_, ["my_page", ..]) => match p.param("pid").or_else(|| p.param("piaproId")) {
            Some(name) => found.profile(format!("https://piapro.jp/{name}")),
            None => found,
        },
        (_, [name, ..]) if !RESERVED.contains(name) => {
            found.profile(format!("https://piapro.jp/{name}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn contents_and_creators() {
        page(
            &PIAPRO,
            "http://piapro.jp/t/zXLG/20101206161601",
            "https://piapro.jp/t/zXLG",
        );
        page(
            &PIAPRO,
            "https://piapro.jp/content/zja2063m7x4yfjvk",
            "https://piapro.jp/content/zja2063m7x4yfjvk",
        );
        profile(
            &PIAPRO,
            "https://piapro.jp/my_page/?view=profile&pid=orzkakkokari",
            "https://piapro.jp/orzkakkokari",
        );
        profile(
            &PIAPRO,
            "https://piapro.jp/nibiirooo_",
            "https://piapro.jp/nibiirooo_",
        );
    }
}
