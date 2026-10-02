//! ArtStation: projects (`/artwork/<id>`, `<user>.artstation.com/projects/<id>`),
//! users, and images on its CDN (`…/large/…` samples of `…/original/…`).

use super::parts::Parts;
use super::{ARTSTATION, SourceUrl};

const RESERVED_SUBS: &[&str] = &["", "www", "cdn", "cdna", "cdnb", "cdn-animation"];
const RESERVED: &[&str] = &[
    "about",
    "blogs",
    "challenges",
    "guides",
    "jobs",
    "learning",
    "marketplace",
    "prints",
    "schools",
    "search",
    "studios",
    "subscribe",
    "artwork",
    "projects",
    "artist",
    "users",
    "api",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "artstation.com" {
        return None;
    }
    let found = SourceUrl::of(&ARTSTATION);
    let path = p.path();
    if p.sub.starts_with("cdn") {
        return Some(match path.as_slice() {
            [
                "p",
                "assets",
                kind @ ("images" | "covers"),
                "images",
                dirs @ ..,
                _size,
                file,
            ] => {
                let mut full = format!(
                    "https://cdn.artstation.com/p/assets/{kind}/images/{}/original/{file}",
                    dirs.join("/")
                );
                if let Some(stamp) = p.query().filter(|q| q.chars().all(|c| c.is_ascii_digit())) {
                    full = format!("{full}?{stamp}");
                }
                found.file(full)
            }
            _ => found.file(p.without_query()),
        });
    }
    let user = |name: &str| format!("https://www.artstation.com/{name}");
    let sub = (!RESERVED_SUBS.contains(&p.sub.as_str())).then_some(p.sub.as_str());
    Some(match (sub, path.as_slice()) {
        (sub, ["artwork" | "projects", id]) => {
            let found = found.page(format!("https://www.artstation.com/artwork/{id}"));
            match sub {
                Some(name) => found.profile(user(name)),
                None => found,
            }
        }
        (None, ["artist", name, ..]) => found.profile(user(name)),
        (None, [name, ..]) if !RESERVED.contains(name) => found.profile(user(name)),
        (Some(name), _) => found.profile(user(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn projects_users_and_images() {
        page(
            &ARTSTATION,
            "https://www.artstation.com/artwork/04XA4",
            "https://www.artstation.com/artwork/04XA4",
        );
        page(
            &ARTSTATION,
            "https://dudeunderscore.artstation.com/projects/NoNmD?album_id=23041",
            "https://www.artstation.com/artwork/NoNmD",
        );
        profile(
            &ARTSTATION,
            "https://www.artstation.com/artist/chicle/albums/all/",
            "https://www.artstation.com/chicle",
        );
        profile(
            &ARTSTATION,
            "https://hosi_na.artstation.com",
            "https://www.artstation.com/hosi_na",
        );
        profile(
            &ARTSTATION,
            "http://www.artstation.com/envie_dai/prints",
            "https://www.artstation.com/envie_dai",
        );
        file(
            &ARTSTATION,
            "https://cdna.artstation.com/p/assets/images/images/005/804/224/large/titapa-khemakavat-sa-dui-srevere.jpg?1493887236",
            Some(
                "https://cdn.artstation.com/p/assets/images/images/005/804/224/original/titapa-khemakavat-sa-dui-srevere.jpg?1493887236",
            ),
        );
    }
}
