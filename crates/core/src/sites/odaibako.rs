//! Odaibako: requests ("odai") and answers, users, and answer images.

use super::parts::Parts;
use super::{ODAIBAKO, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "odaibako.net" {
        return None;
    }
    let found = SourceUrl::of(&ODAIBAKO);
    let user = |name: &str| format!("https://odaibako.net/u/{name}");
    Some(match p.path().as_slice() {
        [_, "post_images", name, file] => {
            // `<hash>.jpeg.webp` is a WebP of `<hash>.jpeg`.
            let file = file
                .strip_suffix(".webp")
                .filter(|f| f.contains('.'))
                .unwrap_or(file);
            found
                .file(format!(
                    "https://ccs.odaibako.net/_/post_images/{name}/{file}"
                ))
                .profile(user(name))
        }
        ["odais", id] => found.page(format!("https://odaibako.net/odais/{id}")),
        ["posts", id] => found.page(format!("https://odaibako.net/posts/{id}")),
        ["u", name] => found.profile(user(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn requests_users_and_images() {
        page(
            &ODAIBAKO,
            "https://odaibako.net/posts/01923bc559bc0fd9ac983610d654ea2d",
            "https://odaibako.net/posts/01923bc559bc0fd9ac983610d654ea2d",
        );
        profile(
            &ODAIBAKO,
            "https://odaibako.net/u/aaaaaariko",
            "https://odaibako.net/u/aaaaaariko",
        );
        file(
            &ODAIBAKO,
            "https://ccs.odaibako.net/w=1600/post_images/aaaaaariko/c126b4961cea4a1c9ae016e224db2a62.jpeg.webp",
            Some(
                "https://ccs.odaibako.net/_/post_images/aaaaaariko/c126b4961cea4a1c9ae016e224db2a62.jpeg",
            ),
        );
    }
}
