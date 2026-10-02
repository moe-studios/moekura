//! Artistree commission pages (`artistree.io/<name>`), whose examples are
//! on CloudFront.

use super::parts::Parts;
use super::{ARTISTREE, SourceUrl};

const RESERVED: &[&str] = &[
    "artist-guide",
    "blog",
    "cookie-policy",
    "contact",
    "faq",
    "mission",
    "press",
    "privacy-policy",
    "search",
    "static",
    "team",
    "terms-and-conditions",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let cdn = p.host == "dwxo6p939as9l.cloudfront.net";
    if p.domain != "artistree.io" && !cdn {
        return None;
    }
    let found = SourceUrl::of(&ARTISTREE);
    let profile = |name: &str| format!("https://artistree.io/{name}");
    Some(match p.path().as_slice() {
        [name, ..] if cdn => found.file(p.without_query()).profile(profile(name)),
        [name] if !RESERVED.contains(name) => match p.fragment() {
            Some(commission) => found
                .page(format!("{}#{commission}", profile(name)))
                .profile(profile(name)),
            None => found.profile(profile(name)),
        },
        ["request" | "queue", name] => found.profile(profile(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn profiles_and_commissions() {
        profile(
            &ARTISTREE,
            "https://artistree.io/crestfallen163",
            "https://artistree.io/crestfallen163",
        );
        profile(
            &ARTISTREE,
            "https://artistree.io/request/adolfozapp",
            "https://artistree.io/adolfozapp",
        );
        page(
            &ARTISTREE,
            "https://artistree.io/crestfallen163#d2ca3306-0a5d-426e-925a-191593e6cfe1",
            "https://artistree.io/crestfallen163#d2ca3306-0a5d-426e-925a-191593e6cfe1",
        );
    }
}
