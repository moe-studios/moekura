//! Privatter: images (`privatter.net/i/<id>`), posts, users, and files on
//! CloudFront.

use super::parts::{Parts, is_digits};
use super::{PRIVATTER, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let cdn = p.host == "d2pqhom6oey9wx.cloudfront.net";
    if p.domain != "privatter.net" && !cdn {
        return None;
    }
    let found = SourceUrl::of(&PRIVATTER);
    Some(match p.path().as_slice() {
        ["img_resize" | "img_original", file] if cdn => found.file(format!(
            "https://d2pqhom6oey9wx.cloudfront.net/img_original/{file}"
        )),
        _ if cdn => found.file(None),
        ["i", id] if is_digits(id) => found.page(format!("https://privatter.net/i/{id}")),
        ["p", id] if is_digits(id) => found.page(format!("https://privatter.net/p/{id}")),
        ["u" | "m", name] => found.profile(format!("https://privatter.net/u/{name}")),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn images_users_and_files() {
        page(
            &PRIVATTER,
            "https://privatter.net/i/7184521",
            "https://privatter.net/i/7184521",
        );
        profile(
            &PRIVATTER,
            "https://privatter.net/m/minami_152133",
            "https://privatter.net/u/minami_152133",
        );
        file(
            &PRIVATTER,
            "https://d2pqhom6oey9wx.cloudfront.net/img_resize/6501563076473624f29c22.png",
            Some("https://d2pqhom6oey9wx.cloudfront.net/img_original/6501563076473624f29c22.png"),
        );
    }
}
