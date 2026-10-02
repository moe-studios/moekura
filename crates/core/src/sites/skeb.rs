//! Skeb commissions: works (`skeb.jp/@<creator>/works/<n>`), creators,
//! and the previews on imgix.

use super::parts::Parts;
use super::{SKEB, SourceUrl};

const RESERVED: &[&str] = &[
    "works", "users", "about", "terms", "creator", "client", "company", "api",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let files = matches!(p.host.as_str(), "skeb.imgix.net" | "si.imgix.net")
        || p.host == "skeb-production.s3.ap-northeast-1.amazonaws.com";
    if p.domain != "skeb.jp" && !files {
        return None;
    }
    let found = SourceUrl::of(&SKEB);
    if files || matches!(p.sub.as_str(), "fcdn" | "cdn" | "si") {
        return Some(found.file(None));
    }
    let creator = |name: &str| format!("https://skeb.jp/@{name}");
    Some(match p.path().as_slice() {
        [name, "works", n] if name.starts_with('@') => {
            let name = &name[1..];
            found
                .page(format!("{}/works/{n}", creator(name)))
                .profile(creator(name))
        }
        ["works", n] => found.page(format!("https://skeb.jp/works/{n}")),
        [name, ..] if name.starts_with('@') => found.profile(creator(&name[1..])),
        [name, ..] if !RESERVED.contains(name) => found.profile(creator(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn works_and_creators() {
        page(
            &SKEB,
            "https://skeb.jp/@OrvMZ/works/3",
            "https://skeb.jp/@OrvMZ/works/3",
        );
        profile(
            &SKEB,
            "https://skeb.jp/@okku_oxn/works",
            "https://skeb.jp/@okku_oxn",
        );
        profile(&SKEB, "https://skeb.jp/OrvMZ", "https://skeb.jp/@OrvMZ");
        file(
            &SKEB,
            "https://skeb.imgix.net/requests/199886_0?bg=%23fff&auto=format&w=800",
            None,
        );
    }
}
