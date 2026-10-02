//! Newgrounds: art (`/art/view/<user>/<title>`), movies, users
//! (`<user>.newgrounds.com`), and files on ngfiles.com.

use super::parts::Parts;
use super::{NEWGROUNDS, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "newgrounds.com" | "ngfiles.com" | "ungrounded.net"
    ) {
        return None;
    }
    let found = SourceUrl::of(&NEWGROUNDS);
    let user = |name: &str| format!("https://{name}.newgrounds.com");
    let path = p.path();
    if p.domain != "newgrounds.com" {
        // images/<dir>/<id>_<user>_<title>.<ext> (old), or
        // images/<dir>/<project>_<image>_<user>_<title>.<hash>.<ext>.
        let artist = match path.as_slice() {
            ["images" | "medium_views", _, file] => {
                let pieces: Vec<&str> = file.splitn(4, '_').collect();
                match pieces.as_slice() {
                    [a, b, name, _] if super::parts::is_digits(a) && super::parts::is_digits(b) => {
                        Some(*name)
                    }
                    [a, name, ..] if super::parts::is_digits(a) => Some(*name),
                    _ => None,
                }
            }
            _ => None,
        };
        let full = match path.as_slice() {
            ["alternate", dir, file] => {
                // `.1080p.mp4` is a smaller version of `.mp4`.
                let file = match file.split_once('.') {
                    Some((stem, rest)) if rest.ends_with("p.mp4") => format!("{stem}.mp4"),
                    _ => (*file).to_owned(),
                };
                Some(format!(
                    "https://uploads.ungrounded.net/alternate/{dir}/{file}"
                ))
            }
            ["images" | "thumbnails" | "comments", ..] => Some(p.as_str().to_owned()),
            _ => None,
        };
        return Some(found.file(full).profile(artist.map(user)));
    }
    if !matches!(p.sub.as_str(), "" | "www") {
        return Some(found.profile(user(&p.sub)));
    }
    Some(match path.as_slice() {
        ["art", "view", name, title] => found
            .page(format!(
                "https://www.newgrounds.com/art/view/{name}/{title}"
            ))
            .profile(user(name)),
        ["portal", "view" | "video", id] => {
            found.page(format!("https://www.newgrounds.com/portal/view/{id}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, on, page, profile};
    use super::*;

    #[test]
    fn art_users_and_files() {
        page(
            &NEWGROUNDS,
            "https://www.newgrounds.com/art/view/puddbytes/costanza-at-bat",
            "https://www.newgrounds.com/art/view/puddbytes/costanza-at-bat",
        );
        page(
            &NEWGROUNDS,
            "https://www.newgrounds.com/portal/video/536659",
            "https://www.newgrounds.com/portal/view/536659",
        );
        profile(
            &NEWGROUNDS,
            "https://natthelich.newgrounds.com/art/",
            "https://natthelich.newgrounds.com",
        );
        file(
            &NEWGROUNDS,
            "https://uploads.ungrounded.net/alternate/1801000/1801343_alternate_165104.720p.mp4?1639666238",
            Some("https://uploads.ungrounded.net/alternate/1801000/1801343_alternate_165104.mp4"),
        );
        let art = on(
            &NEWGROUNDS,
            "https://art.ngfiles.com/images/1254000/1254722_natthelich_pandora.jpg",
        );
        assert_eq!(
            art.profile_url.as_deref(),
            Some("https://natthelich.newgrounds.com")
        );
    }
}
