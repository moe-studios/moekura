//! Huajia (NetEase): works, goods, commissions and character settings on
//! huajia.163.com, profiles, and images.

use super::parts::Parts;
use super::{HUAJIA, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.host.as_str(),
        "huajia.163.com" | "huajia.fp.ps.netease.com"
    ) {
        return None;
    }
    let found = SourceUrl::of(&HUAJIA);
    if p.host == "huajia.fp.ps.netease.com" {
        return Some(found.file(p.without_query()));
    }
    let base = "https://huajia.163.com/main";
    Some(match p.path().as_slice() {
        ["main", "works", id] => found.page(format!("{base}/works/{id}")),
        [
            "main",
            kind @ ("goods" | "projects" | "characterSetting"),
            "details",
            id,
        ] => found.page(format!("{base}/{kind}/details/{id}")),
        ["main", "profile", id] => found.profile(format!("{base}/profile/{id}")),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn works_and_profiles() {
        page(
            &HUAJIA,
            "https://huajia.163.com/main/works/8z4GdKoE",
            "https://huajia.163.com/main/works/8z4GdKoE",
        );
        page(
            &HUAJIA,
            "https://huajia.163.com/main/goods/details/brOjJVME",
            "https://huajia.163.com/main/goods/details/brOjJVME",
        );
        profile(
            &HUAJIA,
            "https://huajia.163.com/main/profile/08nqxj4r?type=Works",
            "https://huajia.163.com/main/profile/08nqxj4r",
        );
        file(
            &HUAJIA,
            "https://huajia.fp.ps.netease.com/file/664ae65bd56ea97215dc3e25JM5jBQGB05?fop=imageView/2/w/300/f/webp",
            Some("https://huajia.fp.ps.netease.com/file/664ae65bd56ea97215dc3e25JM5jBQGB05"),
        );
    }
}
