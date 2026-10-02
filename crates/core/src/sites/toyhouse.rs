//! Toyhouse: images (`toyhou.se/~images/<id>`), characters and their
//! galleries, forum posts, users, and files.

use super::parts::{Parts, is_digits};
use super::{SourceUrl, TOYHOUSE};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "toyhou.se" {
        return None;
    }
    let found = SourceUrl::of(&TOYHOUSE);
    let base = "https://toyhou.se";
    // `<id>.<name>`: a character or gallery.
    let named = |s: &str| s.split_once('.').is_some_and(|(id, _)| is_digits(id));
    let image = p.fragment().filter(|f| is_digits(f));
    let path = p.path();
    if matches!(p.sub.as_str(), "f2" | "file") {
        let id = p
            .stem()
            .and_then(|s| s.split('_').next())
            .filter(|id| is_digits(id))
            .filter(|_| {
                path.iter()
                    .any(|s| matches!(*s, "images" | "thumbnails" | "watermarks"))
            });
        return Some(
            found
                .file(None)
                .page(id.map(|id| format!("{base}/~images/{id}"))),
        );
    }
    Some(match path.as_slice() {
        ["~images", id] => found.page(format!("{base}/~images/{id}")),
        [character, gallery, id] if named(character) && named(gallery) && is_digits(id) => {
            found.page(format!("{base}/{character}/{gallery}/{id}"))
        }
        [character, gallery] if named(character) && named(gallery) => {
            found.page(format!("{base}/{character}/{gallery}"))
        }
        [character, id] if named(character) && is_digits(id) => {
            found.page(format!("{base}/{character}/{id}"))
        }
        [character, ..] if named(character) => found.page(format!("{base}/{character}")),
        ["~forums", board, forum] if named(board) && named(forum) => {
            found.page(format!("{base}/~forums/{board}/{forum}"))
        }
        [name, ..] if !name.starts_with('~') => {
            let found = found.profile(format!("{base}/{name}"));
            match image {
                Some(id) => found.page(format!("{base}/~images/{id}")),
                None => found,
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
    fn images_characters_and_users() {
        page(
            &TOYHOUSE,
            "https://toyhou.se/~images/58037599",
            "https://toyhou.se/~images/58037599",
        );
        page(
            &TOYHOUSE,
            "https://toyhou.se/2712983.cudlil/19136842.reference-sheet/73741617",
            "https://toyhou.se/2712983.cudlil/19136842.reference-sheet/73741617",
        );
        page(
            &TOYHOUSE,
            "https://toyhou.se/19108771.june-human-/gallery",
            "https://toyhou.se/19108771.june-human-",
        );
        profile(
            &TOYHOUSE,
            "https://toyhou.se/427Deer",
            "https://toyhou.se/427Deer",
        );
    }
}
