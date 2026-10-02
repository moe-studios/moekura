//! TikTok: videos and photo posts (`tiktok.com/@<user>/video/<id>`) and
//! users.

use super::parts::Parts;
use super::{SourceUrl, TIKTOK};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "tiktok.com" | "tiktokcdn-us.com" | "tiktokcdn.com"
    ) {
        return None;
    }
    let found = SourceUrl::of(&TIKTOK);
    if p.domain != "tiktok.com" {
        return Some(found.file(None));
    }
    let user = |name: &str| format!("https://www.tiktok.com/{name}");
    Some(match p.path().as_slice() {
        [name, kind @ ("video" | "photo"), id] if name.starts_with('@') => found
            .page(format!("{}/{kind}/{id}", user(name)))
            .profile(user(name)),
        [name] if name.starts_with('@') => found.profile(user(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn videos_and_users() {
        page(
            &TIKTOK,
            "https://www.tiktok.com/@pyromannce/photo/7584709238878915858?_r=1",
            "https://www.tiktok.com/@pyromannce/photo/7584709238878915858",
        );
        profile(
            &TIKTOK,
            "https://www.tiktok.com/@h.panda_12",
            "https://www.tiktok.com/@h.panda_12",
        );
    }
}
