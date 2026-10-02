//! Plurk: plurks (`plurk.com/p/<id>`), users, and images.

use super::parts::Parts;
use super::{PLURK, SourceUrl};

const RESERVED: &[&str] = &[
    "aboutUs",
    "app",
    "brandInfo",
    "contact",
    "content-policy",
    "f",
    "help",
    "hotlinks",
    "login",
    "logout",
    "m",
    "news",
    "p",
    "portal",
    "privacy",
    "qrcode",
    "s",
    "search",
    "settings",
    "signup",
    "terms",
    "top",
    "u",
    "EmoticonManager2",
    "Friends",
    "Photos",
    "UserRecommend",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "plurk.com" {
        return None;
    }
    let found = SourceUrl::of(&PLURK);
    if p.host == "images.plurk.com" {
        // `mx_<id>` is a sample of `<id>`.
        let base = p.basename().unwrap_or_default();
        let base = base.strip_prefix("mx_").unwrap_or(base);
        return Some(found.file(format!("https://images.plurk.com/{base}")));
    }
    let plurk = |id: &str| match p.param("r") {
        Some(r) => format!("https://www.plurk.com/p/{id}?r={r}"),
        None => format!("https://www.plurk.com/p/{id}"),
    };
    let user = |name: &str| format!("https://www.plurk.com/{name}");
    Some(match p.path().as_slice() {
        ["p", id] | ["m" | "s", "p", id] => found.page(plurk(id)),
        ["m" | "s", "u", name, ..] | ["m" | "u", name, ..] if !RESERVED.contains(name) => {
            found.profile(user(name))
        }
        [name, ..] if !RESERVED.contains(name) => found.profile(user(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn plurks_users_and_images() {
        page(
            &PLURK,
            "https://www.plurk.com/m/p/okxzae?r=7590694648",
            "https://www.plurk.com/p/okxzae?r=7590694648",
        );
        profile(
            &PLURK,
            "https://www.plurk.com/m/u/leiy1225/fans",
            "https://www.plurk.com/leiy1225",
        );
        profile(
            &PLURK,
            "https://www.plurk.com/redeyehare",
            "https://www.plurk.com/redeyehare",
        );
        file(
            &PLURK,
            "https://images.plurk.com/mx_5wj6WD0r6y4rLN0DL3sqag.jpg",
            Some("https://images.plurk.com/5wj6WD0r6y4rLN0DL3sqag.jpg"),
        );
    }
}
