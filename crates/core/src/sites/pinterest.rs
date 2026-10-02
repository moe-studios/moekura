//! Pinterest: pins (`pinterest.com/pin/<id>/`), users, and images on
//! pinimg.com (`/736x/` samples of `/originals/`).

use super::parts::Parts;
use super::{PINTEREST, SourceUrl};

const RESERVED: &[&str] = &[
    "docs", "ideas", "pin", "resource", "shopping", "today", "videos", "_", "search",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let pinterest = p.domain.split('.').next() == Some("pinterest");
    if !pinterest && !matches!(p.domain.as_str(), "pinimg.com" | "pin.it") {
        return None;
    }
    let found = SourceUrl::of(&PINTEREST);
    let path = p.path();
    if p.domain == "pinimg.com" {
        return Some(match path.as_slice() {
            ["originals", ..] => found.file(p.without_query()),
            _ => found.file(None),
        });
    }
    if p.domain == "pin.it" {
        return Some(found);
    }
    Some(match path.as_slice() {
        ["pin", id, ..] => found.page(format!("https://www.pinterest.com/pin/{id}/")),
        [name, ..] if !RESERVED.contains(name) && p.sub != "api" => {
            found.profile(format!("https://www.pinterest.com/{name}/"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn pins_users_and_images() {
        page(
            &PINTEREST,
            "https://www.pinterest.jp/pin/551409548144250908/",
            "https://www.pinterest.com/pin/551409548144250908/",
        );
        profile(
            &PINTEREST,
            "https://www.pinterest.com/uchihajake/hands/",
            "https://www.pinterest.com/uchihajake/",
        );
        file(
            &PINTEREST,
            "https://i.pinimg.com/originals/a7/7c/67/a77c67f95a4fec64de7969e98f29cf3b.png",
            Some("https://i.pinimg.com/originals/a7/7c/67/a77c67f95a4fec64de7969e98f29cf3b.png"),
        );
    }
}
