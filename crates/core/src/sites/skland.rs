//! Skland (Hypergryph's forum): articles, profiles, and images on
//! hycdn.cn.

use super::parts::Parts;
use super::{SKLAND, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "skland.com" | "hycdn.cn") {
        return None;
    }
    let found = SourceUrl::of(&SKLAND);
    if p.domain == "hycdn.cn" {
        return Some(match p.sub.as_str() {
            "bbs" => found.file(p.without_query()),
            "skland-vod" => found.file(p.as_str().to_owned()),
            _ => found.file(None),
        });
    }
    let id = p.param("id");
    Some(match (p.path().as_slice(), id) {
        (["article"] | ["h", "detail"], Some(id)) => {
            found.page(format!("https://www.skland.com/article?id={id}"))
        }
        (["profile"], Some(id)) => found.profile(format!("https://www.skland.com/profile?id={id}")),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn articles_profiles_and_images() {
        page(
            &SKLAND,
            "https://m.skland.com/article?id=1827735",
            "https://www.skland.com/article?id=1827735",
        );
        page(
            &SKLAND,
            "https://www.skland.com/h/detail?id=611376",
            "https://www.skland.com/article?id=611376",
        );
        profile(
            &SKLAND,
            "https://www.skland.com/profile?id=4040407836824",
            "https://www.skland.com/profile?id=4040407836824",
        );
        file(
            &SKLAND,
            "https://bbs.hycdn.cn/image/2024/04/29/576904/1dc98f0a6780ddcbc107d77bfdba673f.webp?x-oss-process=style/item_style",
            Some(
                "https://bbs.hycdn.cn/image/2024/04/29/576904/1dc98f0a6780ddcbc107d77bfdba673f.webp",
            ),
        );
    }
}
