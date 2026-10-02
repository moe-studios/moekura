//! TINAMI: works (`tinami.com/view/<id>`), creators, and images.

use super::parts::Parts;
use super::{SourceUrl, TINAMI};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "tinami.com" | "tinami.jp") {
        return None;
    }
    let found = SourceUrl::of(&TINAMI);
    if p.sub == "img" {
        return Some(found.file(None));
    }
    let creator = |id: &str| format!("https://www.tinami.com/creator/profile/{id}");
    Some(match p.path().as_slice() {
        ["view", "tweet", "card", id] => found
            .file(None)
            .page(format!("https://www.tinami.com/view/{id}")),
        ["view", id] => found.page(format!("https://www.tinami.com/view/{id}")),
        ["creator", "profile", id] => found.profile(creator(id)),
        ["search", "list"] => match p.param("prof_id") {
            Some(id) => found.profile(creator(&id)),
            None => found,
        },
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn works_and_creators() {
        page(
            &TINAMI,
            "https://www.tinami.com/view/461459",
            "https://www.tinami.com/view/461459",
        );
        profile(
            &TINAMI,
            "http://www.tinami.com/creator/profile/1624",
            "https://www.tinami.com/creator/profile/1624",
        );
        profile(
            &TINAMI,
            "https://www.tinami.com/search/list?prof_id=1624",
            "https://www.tinami.com/creator/profile/1624",
        );
    }
}
