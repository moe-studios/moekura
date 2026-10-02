//! Patreon: posts (`patreon.com/posts/<title>-<id>`), shop items,
//! creators, and media on patreonusercontent.com.

use super::parts::{Parts, is_digits};
use super::{PATREON, SourceUrl};

const RESERVED: &[&str] = &[
    "api",
    "bePatron",
    "c",
    "cw",
    "card-teaser-image",
    "collection",
    "checkout",
    "file",
    "home",
    "join",
    "login",
    "m",
    "messages",
    "notifications",
    "policy",
    "posts",
    "profile",
    "search",
    "settings",
    "user",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "patreon.com" | "patreonusercontent.com") {
        return None;
    }
    let found = SourceUrl::of(&PATREON);
    let base = "https://www.patreon.com";
    let path = p.path();
    if p.domain == "patreonusercontent.com" {
        return Some(match path.as_slice() {
            [_, "patreon-media", "p", "post", id, ..] => {
                found.file(None).page(format!("{base}/posts/{id}"))
            }
            [_, "patreon-media", "p", "user", id, ..] => {
                found.file(None).profile(format!("{base}/user?u={id}"))
            }
            _ => found.file(None),
        });
    }
    let creator = |name: &str| format!("{base}/{name}");
    // `<title>-<id>` or `<id>`.
    let post_id = |slug: &str| {
        let id = slug.rsplit('-').next().unwrap_or(slug);
        is_digits(id).then(|| id.to_owned())
    };
    Some(match path.as_slice() {
        ["posts", slug, ..] | ["m", "posts", slug, ..] => match post_id(slug) {
            Some(_) if slug.contains('-') => found.page(format!("{base}/posts/{slug}")),
            Some(id) => found.page(format!("{base}/posts/{id}")),
            None => found,
        },
        ["api", "posts", id] => found.page(format!("{base}/posts/{id}")),
        ["api", "user", id] => found.profile(format!("{base}/user?u={id}")),
        ["file"] => match p.param("h") {
            Some(id) => found.file(None).page(format!("{base}/posts/{id}")),
            None => found.file(None),
        },
        ["checkout" | "join" | "m" | "c" | "cw", name, ..] if !RESERVED.contains(name) => {
            found.profile(creator(name))
        }
        [name, "shop", slug, ..] if !RESERVED.contains(name) => found
            .page(format!("{}/shop/{slug}", creator(name)))
            .profile(creator(name)),
        [name, "posts", slug] if !RESERVED.contains(name) && post_id(slug).is_some() => found
            .page(format!("{}/posts/{slug}", creator(name)))
            .profile(creator(name)),
        _ if p.param("u").is_some() => found.profile(format!(
            "{base}/user?u={}",
            p.param("u").unwrap_or_default()
        )),
        [name, ..] if !RESERVED.contains(name) => found.profile(creator(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn posts_and_creators() {
        page(
            &PATREON,
            "https://www.patreon.com/posts/sparkle-71057815",
            "https://www.patreon.com/posts/sparkle-71057815",
        );
        page(
            &PATREON,
            "https://www.patreon.com/api/posts/71057815",
            "https://www.patreon.com/posts/71057815",
        );
        page(
            &PATREON,
            "https://www.patreon.com/PrincessHinghoi/posts/yada-yada-desu-160554516",
            "https://www.patreon.com/PrincessHinghoi/posts/yada-yada-desu-160554516",
        );
        profile(
            &PATREON,
            "https://www.patreon.com/1041uuu/about",
            "https://www.patreon.com/1041uuu",
        );
        profile(
            &PATREON,
            "https://www.patreon.com/bePatron?u=4045578",
            "https://www.patreon.com/user?u=4045578",
        );
    }
}
