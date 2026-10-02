//! VK: wall posts and photos (`vk.com/wall-<owner>_<id>`), articles,
//! communities and users, and images on userapi.com.

use super::parts::Parts;
use super::{SourceUrl, VK};

const RESERVED: &[&str] = &[
    "about",
    "audio",
    "away.php",
    "blog",
    "clips",
    "games",
    "groups",
    "feed",
    "jobs",
    "join",
    "legal",
    "login",
    "mobile",
    "products",
    "technology",
    "services",
    "terms",
    "video",
];
const PAGE_TYPES: &[&str] = &[
    "album", "albums", "audio", "audios", "clip", "club", "doc", "event", "id", "market", "page",
    "photo", "post", "product", "public", "topic", "uslugi", "video", "videos", "wall", "wpt",
];

/// `wall-111670353_64474`: the kind, owner and item.
fn page_id(s: &str) -> Option<(&str, &str, Option<&str>)> {
    let kind = PAGE_TYPES
        .iter()
        .filter(|k| s.starts_with(**k))
        .max_by_key(|k| k.len())?;
    let rest = &s[kind.len()..];
    let (owner, item) = match rest.split_once('_') {
        Some((owner, item)) => (owner, Some(item)),
        None => (rest, None),
    };
    let digits = owner.strip_prefix('-').unwrap_or(owner);
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then_some((
        *kind,
        owner,
        item.filter(|i| i.chars().all(|c| c.is_ascii_digit())),
    ))
}

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "vk.com" | "vk.cc" | "vk.me" | "vk.ru" | "vkontakte.ru" | "userapi.com"
    ) {
        return None;
    }
    let found = SourceUrl::of(&VK);
    let path = p.path();
    if p.domain == "userapi.com" {
        return Some(match path.as_slice() {
            ["impf" | "impg", rest @ ..] => {
                found.file(format!("https://pp.userapi.com/{}", rest.join("/")))
            }
            ["s", "v1", ..] => {
                let mut url = p.url.clone();
                let pairs: Vec<(String, String)> = url
                    .query_pairs()
                    .filter(|(k, _)| k != "cs")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                url.query_pairs_mut()
                    .clear()
                    .extend_pairs(pairs)
                    .append_pair("cs", "99999x99999");
                found.file(url.to_string())
            }
            _ if p.sub.starts_with("sun") || p.sub.starts_with("psv") => {
                found.file(format!("https://pp.userapi.com{}", p.url.path()))
            }
            _ => found.file(None),
        });
    }
    if !matches!(p.domain.as_str(), "vk.com" | "vk.ru" | "vkontakte.ru") {
        return Some(found);
    }
    let base = "https://vk.com";
    let with_id = |found: SourceUrl, id: &str| match page_id(id) {
        Some(("id", _, _)) => found.profile(format!("{base}/{id}")),
        Some((_, owner, Some(_))) => found
            .page(format!("{base}/{id}"))
            .profile(format!("{base}/wall{owner}")),
        Some((_, owner, None)) => found.profile(format!("{base}/wall{owner}")),
        None => found,
    };
    // An item shown over the page (`?z=photo-1_2`, `?w=…`).
    let overlay = p
        .param("z")
        .or_else(|| p.param("w"))
        .map(|z| z.split('/').next().unwrap_or_default().to_owned());
    Some(match path.as_slice() {
        [name] if name.starts_with('@') => match name.split_once('-') {
            Some(_) => found.page(format!("{base}/{name}")),
            None => found.profile(format!("{base}/{}", &name[1..])),
        },
        ["video", name] if name.starts_with('@') => found.profile(format!("{base}/{}", &name[1..])),
        ["clips", name] => found.profile(format!("{base}/{name}")),
        [first, ..] if page_id(first).is_some() => {
            let found = with_id(found, first);
            match overlay {
                Some(item) => with_id(found, &item),
                None => found,
            }
        }
        [name, ..] if !RESERVED.contains(name) => {
            let found = found.profile(format!("{base}/{name}"));
            match overlay {
                Some(item) if page_id(&item).is_some_and(|(_, _, i)| i.is_some()) => {
                    found.page(format!("{base}/{item}"))
                }
                _ => found,
            }
        }
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
            &VK,
            "https://vk.com/wall-111670353_64474",
            "https://vk.com/wall-111670353_64474",
        );
        page(
            &VK,
            "https://vk.com/sgips?z=photo-111670353_457239029",
            "https://vk.com/photo-111670353_457239029",
        );
        page(
            &VK,
            "https://vk.com/@sgips-tri-istorii-o-lovce",
            "https://vk.com/@sgips-tri-istorii-o-lovce",
        );
        profile(
            &VK,
            "https://vk.com/enigmasblog/Fullart",
            "https://vk.com/enigmasblog",
        );
        profile(
            &VK,
            "https://vk.com/wall-111670353",
            "https://vk.com/wall-111670353",
        );
        file(
            &VK,
            "https://sun9-69.userapi.com/impg/VJBWV0vYZJLRhFBkQxaVtVo9_givXP6BycJJow/RBoOQ0nHMNc.jpg?size=1200x1600",
            Some("https://pp.userapi.com/VJBWV0vYZJLRhFBkQxaVtVo9_givXP6BycJJow/RBoOQ0nHMNc.jpg"),
        );
    }
}
