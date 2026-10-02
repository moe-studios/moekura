//! Imgur: images (`imgur.com/<id>`, `i.imgur.com/<id>.jpg`), albums and
//! galleries, and users.

use super::parts::Parts;
use super::{IMGUR, SourceUrl};

/// An image id: 5 or 7 characters (a sixth or eighth marks a sample).
fn image_id(stem: &str) -> Option<&str> {
    if !stem.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    match stem.len() {
        5 | 7 => Some(stem),
        6 => Some(&stem[..5]),
        8 => Some(&stem[..7]),
        _ => None,
    }
}

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "imgur.com" | "imgur.io") {
        return None;
    }
    let found = SourceUrl::of(&IMGUR);
    let page = |id: &str| format!("https://imgur.com/{id}");
    let album = |id: &str| format!("https://imgur.com/a/{id}");
    Some(match p.path().as_slice() {
        [_] if p.ext().is_some() => {
            let ext = p.ext().unwrap_or_default();
            let ext = if ext == "gifv" { "gif".to_owned() } else { ext };
            match p.stem().and_then(image_id) {
                Some(id) => found
                    .file(format!("https://i.imgur.com/{id}.{ext}"))
                    .page(page(id)),
                None => found.file(None),
            }
        }
        ["download", id, ..] => found
            .file(format!("https://imgur.com/download/{id}"))
            .page(page(id)),
        ["gallery" | "a", id, ..] => found.page(album(id)),
        ["t", _, id] => found.page(album(id)),
        ["user", name, ..] => found.profile(format!("https://imgur.com/user/{name}")),
        [slug] => {
            // `title-<id>`: the id is what follows the last dash.
            let id = slug.rsplit('-').next().unwrap_or(slug);
            match image_id(id).filter(|i| i.len() == id.len()) {
                Some(_) => found.page(page(slug)),
                None => found,
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
    fn images_albums_and_users() {
        page(
            &IMGUR,
            "https://imgur.io/c7EXjJu",
            "https://imgur.com/c7EXjJu",
        );
        page(
            &IMGUR,
            "https://imgur.com/gallery/0BDNq",
            "https://imgur.com/a/0BDNq",
        );
        profile(
            &IMGUR,
            "https://imgur.com/user/naugrim2875/posts",
            "https://imgur.com/user/naugrim2875",
        );
        file(
            &IMGUR,
            "https://i.imgur.com/c7EXjJuh.jpeg",
            Some("https://i.imgur.com/c7EXjJu.jpeg"),
        );
        file(
            &IMGUR,
            "https://i.imgur.com/c7EXjJu.gifv",
            Some("https://i.imgur.com/c7EXjJu.gif"),
        );
    }
}
