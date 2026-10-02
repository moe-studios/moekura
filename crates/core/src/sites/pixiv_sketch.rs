//! Pixiv Sketch: items (`sketch.pixiv.net/items/<id>`), users and their
//! images.

use super::parts::Parts;
use super::{PIXIV_SKETCH, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.host.as_str(),
        "sketch.pixiv.net" | "img-sketch.pixiv.net" | "img-sketch.pximg.net"
    ) {
        return None;
    }
    let found = SourceUrl::of(&PIXIV_SKETCH);
    let path = p.path();
    if p.host != "sketch.pixiv.net" {
        let at = path.iter().position(|s| *s == "uploads");
        return Some(match at.map(|at| &path[at..]) {
            Some(["uploads", "medium", "file", dir, file]) => found.file(format!(
                "https://img-sketch.pixiv.net/uploads/medium/file/{dir}/{file}"
            )),
            _ => found.file(None),
        });
    }
    Some(match path.as_slice() {
        ["items", id] => found.page(format!("https://sketch.pixiv.net/items/{id}")),
        [name, ..] if name.starts_with('@') => {
            found.profile(format!("https://sketch.pixiv.net/{name}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn items_users_and_images() {
        page(
            &PIXIV_SKETCH,
            "https://sketch.pixiv.net/items/5835314698645024323",
            "https://sketch.pixiv.net/items/5835314698645024323",
        );
        profile(
            &PIXIV_SKETCH,
            "https://sketch.pixiv.net/@user_ejkv8372/followings",
            "https://sketch.pixiv.net/@user_ejkv8372",
        );
        file(
            &PIXIV_SKETCH,
            "https://img-sketch.pximg.net/c!/w=540,f=webp:jpeg/uploads/medium/file/4463372/8906921629213362989.jpg",
            Some(
                "https://img-sketch.pixiv.net/uploads/medium/file/4463372/8906921629213362989.jpg",
            ),
        );
    }
}
