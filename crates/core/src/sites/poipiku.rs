//! Poipiku: posts (`poipiku.com/<user>/<post>.html`), users, and images.

use super::parts::{Parts, is_digits, leading_digits};
use super::{POIPIKU, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "poipiku.com" {
        return None;
    }
    let found = SourceUrl::of(&POIPIKU);
    // Ids are zero-padded in file paths.
    let trim = |id: &str| {
        let id = id.trim_start_matches('0');
        if id.is_empty() {
            "0".to_owned()
        } else {
            id.to_owned()
        }
    };
    let post =
        |user: &str, id: &str| format!("https://poipiku.com/{}/{}.html", trim(user), trim(id));
    let user = |id: &str| format!("https://poipiku.com/{}/", trim(id));
    let path = p.path();
    if matches!(p.sub.as_str(), "img" | "img-org" | "cdn") {
        let (owner, file) = match path.as_slice() {
            [_, owner, file] | [owner, file] => (*owner, *file),
            _ => return Some(found.file(None)),
        };
        let found = found.file(None).profile(user(owner));
        return Some(match leading_digits(file) {
            Some(id) => found.page(post(owner, id)),
            None => found,
        });
    }
    Some(match path.as_slice() {
        [owner, file] if is_digits(owner) && file.ends_with(".html") => {
            match leading_digits(file) {
                Some(id) => found.page(post(owner, id)).profile(user(owner)),
                None => found.profile(user(owner)),
            }
        }
        ["IllustListPcV.jsp" | "IllustListGridPcV.jsp" | "ActivityListPcV.jsp"] => {
            match p.param("ID") {
                Some(id) => found.profile(user(&id)),
                None => found,
            }
        }
        [owner] if is_digits(owner) => found.profile(user(owner)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{on, page, profile};
    use super::*;

    #[test]
    fn posts_users_and_images() {
        page(
            &POIPIKU,
            "https://poipiku.com/6849873/8271386.html",
            "https://poipiku.com/6849873/8271386.html",
        );
        profile(
            &POIPIKU,
            "https://poipiku.com/IllustListPcV.jsp?ID=9056",
            "https://poipiku.com/9056/",
        );
        let image = on(
            &POIPIKU,
            "https://img.poipiku.com/user_img02/006849873/008271386_016865825_S968sAh7Y.jpeg_640.jpg",
        );
        assert!(image.is_file);
        assert_eq!(
            image.page_url.as_deref(),
            Some("https://poipiku.com/6849873/8271386.html")
        );
    }
}
