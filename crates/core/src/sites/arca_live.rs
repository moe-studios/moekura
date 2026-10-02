//! Arca.live: posts (`arca.live/b/<channel>/<id>`), users, and images on
//! namu.la.

use super::parts::{Parts, is_digits};
use super::{ARCA_LIVE, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "arca.live" | "namu.la") {
        return None;
    }
    let found = SourceUrl::of(&ARCA_LIVE);
    if p.domain == "namu.la" {
        // `?type=orig` asks for the original.
        let mut url = p.url.clone();
        url.set_fragment(None);
        url.query_pairs_mut().clear().append_pair("type", "orig");
        return Some(found.file(url.to_string()));
    }
    let user = |name: &str, id: Option<&str>| match id {
        Some(id) => format!("https://arca.live/u/@{name}/{id}"),
        None => format!("https://arca.live/u/@{name}"),
    };
    Some(match p.path().as_slice() {
        ["b", channel, id] if is_digits(id) => {
            found.page(format!("https://arca.live/b/{channel}/{id}"))
        }
        ["u", name, id] if name.starts_with('@') && is_digits(id) => {
            found.profile(user(&name[1..], Some(id)))
        }
        ["u", name] if name.starts_with('@') => found.profile(user(&name[1..], None)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_users_and_images() {
        page(
            &ARCA_LIVE,
            "https://arca.live/b/arknights/66031722?p=1",
            "https://arca.live/b/arknights/66031722",
        );
        profile(
            &ARCA_LIVE,
            "https://arca.live/u/@Nauju/45320365",
            "https://arca.live/u/@Nauju/45320365",
        );
        file(
            &ARCA_LIVE,
            "https://ac2.namu.la/20221225sac2/e06dcf8edd29c597240898a6752c74dbdd0680fc932cfd0ecc898795f1db34b5.jpg",
            Some(
                "https://ac2.namu.la/20221225sac2/e06dcf8edd29c597240898a6752c74dbdd0680fc932cfd0ecc898795f1db34b5.jpg?type=orig",
            ),
        );
    }
}
