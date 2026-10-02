//! Galleria (emotionflow.com): illustrations
//! (`galleria.emotionflow.com/<user>/<id>.html`), users, and images.

use super::parts::{Parts, is_digits, leading_digits};
use super::{GALLERIA, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "emotionflow.com" {
        return None;
    }
    let found = SourceUrl::of(&GALLERIA);
    let page = |user: &str, id: &str| format!("https://galleria.emotionflow.com/{user}/{id}.html");
    let profile = |user: &str| format!("https://galleria.emotionflow.com/{user}/");
    let path = p.path();
    if let Some(at) = path.iter().position(|s| s.starts_with("user_img")) {
        return Some(match &path[at..] {
            [_, user, _] if is_digits(user) => {
                // `i660870_869.jpeg`: the illustration's id after `i`/`c`.
                let id = p
                    .stem()
                    .and_then(|s| s.strip_prefix(['i', 'c']))
                    .and_then(leading_digits);
                let found = found.file(None).profile(profile(user));
                match id {
                    Some(id) => found.page(page(user, id)),
                    None => found,
                }
            }
            _ => found.file(None),
        });
    }
    let path: Vec<&str> = path.into_iter().filter(|s| *s != "s").collect();
    Some(match path.as_slice() {
        [user, file] if is_digits(user) && file.ends_with(".html") => {
            let id = file.trim_end_matches(".html");
            if is_digits(id) {
                found.page(page(user, id)).profile(profile(user))
            } else {
                found.profile(profile(user))
            }
        }
        ["IllustDetailV.jsp"] => match (p.param("ID"), p.param("TD")) {
            (Some(user), Some(id)) => found.page(page(&user, &id)).profile(profile(&user)),
            _ => found,
        },
        ["GalleryListGridV.jsp" | "MyGalleryListV.jsp"] => match p.param("ID") {
            Some(user) => found.profile(profile(&user)),
            None => found,
        },
        [user, ..] if is_digits(user) => found.profile(profile(user)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{on, page, profile};
    use super::*;

    #[test]
    fn illustrations_users_and_images() {
        page(
            &GALLERIA,
            "https://galleria.emotionflow.com/s/40775/660870.html",
            "https://galleria.emotionflow.com/40775/660870.html",
        );
        page(
            &GALLERIA,
            "https://galleria.emotionflow.com/IllustDetailV.jsp?ID=136703&TD=701021",
            "https://galleria.emotionflow.com/136703/701021.html",
        );
        profile(
            &GALLERIA,
            "https://galleria.emotionflow.com/40775/gallery.html",
            "https://galleria.emotionflow.com/40775/",
        );
        let image = on(
            &GALLERIA,
            "https://galleria-img.emotionflow.com/user_img9/40775/i660870_869.jpeg",
        );
        assert_eq!(
            image.page_url.as_deref(),
            Some("https://galleria.emotionflow.com/40775/660870.html")
        );
    }
}
