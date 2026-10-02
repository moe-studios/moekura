//! Fur Affinity: submissions (`furaffinity.net/view/<id>`), users, and
//! files on d.furaffinity.net.

use super::parts::{Parts, is_digits};
use super::{FURAFFINITY, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "furaffinity.net"
            | "fxraffinity.net"
            | "fxfuraffinity.net"
            | "vxfuraffinity.net"
            | "xfuraffinity.net"
    ) {
        return None;
    }
    let found = SourceUrl::of(&FURAFFINITY);
    let user = |name: &str| format!("https://www.furaffinity.net/user/{name}");
    Some(match p.path().as_slice() {
        ["view" | "full", id] if is_digits(id) => {
            found.page(format!("https://www.furaffinity.net/view/{id}"))
        }
        ["art", name, _, _] => found.file(p.without_query()).profile(user(name)),
        [
            "gallery" | "user" | "favorites" | "scraps" | "journals" | "stats",
            name,
            ..,
        ] => found.profile(user(name)),
        _ if p.has_file_ext() => found.file(None),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn submissions_users_and_files() {
        page(
            &FURAFFINITY,
            "https://www.furaffinity.net/view/46821705/",
            "https://www.furaffinity.net/view/46821705",
        );
        profile(
            &FURAFFINITY,
            "https://www.furaffinity.net/scraps/iwbitu/2/?",
            "https://www.furaffinity.net/user/iwbitu",
        );
        file(
            &FURAFFINITY,
            "https://d.furaffinity.net/art/iwbitu/1650222955/1650222955.iwbitu_yubi.jpg",
            Some("https://d.furaffinity.net/art/iwbitu/1650222955/1650222955.iwbitu_yubi.jpg"),
        );
    }
}
