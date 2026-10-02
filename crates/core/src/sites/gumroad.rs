//! Gumroad: products (`<user>.gumroad.com/l/<id>`), posts, creators, and
//! files on public-files.gumroad.com.

use super::parts::Parts;
use super::{GUMROAD, SourceUrl};

const RESERVED: &[&str] = &[
    "",
    "www",
    "public-files",
    "assets",
    "static-2",
    "app",
    "discover",
    "features",
    "pricing",
    "university",
    "blog",
    "login",
    "signup",
    "terms",
    "privacy",
    "l",
    "p",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "gumroad.com" | "gum.co") {
        return None;
    }
    let found = SourceUrl::of(&GUMROAD);
    if p.domain == "gum.co" {
        return Some(found);
    }
    let path = p.path();
    if p.sub == "public-files" {
        return Some(match path.as_slice() {
            [id] | ["variants", id, _] => {
                found.file(format!("https://public-files.gumroad.com/{id}"))
            }
            _ => found.file(None),
        });
    }
    let user = (!RESERVED.contains(&p.sub.as_str())).then_some(p.sub.as_str());
    let home = |name: &str| format!("https://{name}.gumroad.com");
    Some(match (user, path.as_slice()) {
        (Some(name), ["l", id]) => found
            .page(format!("{}/l/{id}", home(name)))
            .profile(home(name)),
        (None, ["l", id]) => found.page(format!("https://www.gumroad.com/l/{id}")),
        (Some(name), ["p", id]) => found
            .page(format!("{}/p/{id}", home(name)))
            .profile(home(name)),
        (None, [name]) if !RESERVED.contains(name) => found.profile(home(name)),
        (Some(name), _) => found.profile(home(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn products_creators_and_files() {
        page(
            &GUMROAD,
            "https://aiki.gumroad.com/l/HelmV2T3?layout=profile",
            "https://aiki.gumroad.com/l/HelmV2T3",
        );
        page(
            &GUMROAD,
            "https://gumroad.com/l/HelmV2T3",
            "https://www.gumroad.com/l/HelmV2T3",
        );
        profile(
            &GUMROAD,
            "https://www.gumroad.com/aiki",
            "https://aiki.gumroad.com",
        );
        file(
            &GUMROAD,
            "https://public-files.gumroad.com/variants/nsqiekm8gnl5nfrw3mtthminn2ig/e82ce07851bf15f5ab0ebde47958bb04",
            Some("https://public-files.gumroad.com/nsqiekm8gnl5nfrw3mtthminn2ig"),
        );
    }
}
