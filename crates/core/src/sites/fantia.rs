//! Fantia: posts and products (`fantia.jp/posts/<id>`), fan clubs, and
//! files on c.fantia.jp.

use super::parts::{Parts, is_digits};
use super::{FANTIA, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "fantia.jp" {
        return None;
    }
    let found = SourceUrl::of(&FANTIA);
    let post = |id: &str| format!("https://fantia.jp/posts/{id}");
    let product = |id: &str| format!("https://fantia.jp/products/{id}");
    Some(match p.path().as_slice() {
        ["uploads", kind, "file" | "image", id, file] => {
            // `main_<uuid>.jpeg` and the like are samples.
            let sample = file.split_once('_').is_some_and(|(_, rest)| {
                super::parts::is_uuid(rest.split('.').next().unwrap_or(""))
            });
            let full = (!sample).then(|| p.without_query());
            match *kind {
                "post" => found.file(full).page(post(id)),
                "product" => found.file(full).page(product(id)),
                _ => found.file(full),
            }
        }
        ["uploads", ..] => found.file(None),
        ["posts", id, "download" | "album_image", ..] => {
            found.file(p.as_str().to_owned()).page(post(id))
        }
        ["posts", id, ..] if is_digits(id) => found.page(post(id)),
        ["products", id] if is_digits(id) => found.page(product(id)),
        ["fanclubs", id, ..] if is_digits(id) => {
            found.profile(format!("https://fantia.jp/fanclubs/{id}"))
        }
        [name] => found.profile(format!("https://fantia.jp/{name}")),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_clubs_and_files() {
        page(
            &FANTIA,
            "https://fantia.jp/posts/2245222/post_content_photo/14978435",
            "https://fantia.jp/posts/2245222",
        );
        page(
            &FANTIA,
            "https://fantia.jp/products/249638",
            "https://fantia.jp/products/249638",
        );
        profile(
            &FANTIA,
            "https://fantia.jp/fanclubs/1654/posts",
            "https://fantia.jp/fanclubs/1654",
        );
        file(
            &FANTIA,
            "https://c.fantia.jp/uploads/post/file/1070093/main_16faf0b1-58d8-4aac-9e86-b243063eaaf1.jpeg",
            None,
        );
    }
}
