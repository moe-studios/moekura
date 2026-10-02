//! Dotpict: works (`dotpict.net/works/<id>`), users, and images on
//! dotpicko.net.

use super::parts::Parts;
use super::{DOTPICT, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "dotpict.net" | "dotpicko.net") {
        return None;
    }
    let found = SourceUrl::of(&DOTPICT);
    if p.domain == "dotpicko.net" {
        return Some(match p.path().as_slice() {
            ["work", ..] => found.file(p.without_query()),
            _ => found.file(None),
        });
    }
    let work = |id: &str| format!("https://dotpict.net/works/{id}");
    Some(match (p.sub.as_str(), p.path().as_slice()) {
        ("", ["works", id, ..]) => found.page(work(id)),
        ("", ["users", id, ..]) => found.profile(format!("https://dotpict.net/users/{id}")),
        ("", [name]) if name.starts_with('@') => {
            found.profile(format!("https://dotpict.net/{name}"))
        }
        (name, rest) if !name.is_empty() => {
            let found = found.profile(format!("https://dotpict.net/@{name}"));
            match rest {
                ["works", id] => found.page(work(id)),
                _ => found,
            }
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn works_and_users() {
        page(
            &DOTPICT,
            "https://dotpict.net/works/4814277",
            "https://dotpict.net/works/4814277",
        );
        page(
            &DOTPICT,
            "https://jumpanaatta.dotpict.net/works/5356301",
            "https://dotpict.net/works/5356301",
        );
        profile(
            &DOTPICT,
            "https://dotpict.net/users/2011866/followedUsers",
            "https://dotpict.net/users/2011866",
        );
        profile(
            &DOTPICT,
            "https://jumpanaatta.dotpict.net/",
            "https://dotpict.net/@jumpanaatta",
        );
    }
}
