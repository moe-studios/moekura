//! Apple Music: albums and artists (`music.apple.com/<country>/album/…`,
//! the old `itunes.apple.com`), and covers on mzstatic.com.

use super::parts::Parts;
use super::{APPLE_MUSIC, SourceUrl};

/// A cover's largest version: `a<n>.mzstatic.com` URLs are originals;
/// `is<n>-ssl…/image/thumb/<path>/<W>x<H>….webp` thumbnails name theirs.
fn full_cover(p: &Parts) -> Option<String> {
    if p.sub.starts_with('a') {
        return Some(p.without_query());
    }
    let n = p.sub.strip_prefix("is")?.split('-').next()?;
    let path = p.path();
    let ["image", "thumb", rest @ ..] = path.as_slice() else {
        return None;
    };
    let is_size = |s: &str| {
        s.split_once('x').is_some_and(|(w, h)| {
            super::parts::is_digits(w) && h.starts_with(|c: char| c.is_ascii_digit())
        })
    };
    match rest {
        // thumb/<id>/9999x0w.png: only a bigger size of the same.
        [id, size] if is_size(size) && !id.contains('.') => {
            let ext = p.ext().unwrap_or_else(|| "png".into());
            Some(format!(
                "https://{}/image/thumb/{id}/10000x10000.{ext}",
                p.host
            ))
        }
        [dirs @ .., last] => {
            let dirs = if is_size(last) { dirs } else { rest };
            Some(format!(
                "https://a{n}.mzstatic.com/us/r1000/0/{}",
                dirs.join("/")
            ))
        }
        [] => None,
    }
}

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let music = matches!(p.host.as_str(), "music.apple.com" | "itunes.apple.com");
    if !music && p.domain != "mzstatic.com" {
        return None;
    }
    let found = SourceUrl::of(&APPLE_MUSIC);
    if p.domain == "mzstatic.com" {
        return Some(found.file(full_cover(p)));
    }
    let path = p.path();
    // An optional country code comes first.
    let (country, rest) = match path.as_slice() {
        [c, rest @ ..] if c.len() == 2 => (Some(*c), rest),
        rest => (None, rest),
    };
    let join = |kind: &str, name: Option<&str>, id: &str| {
        let id = id.trim_start_matches("id");
        let mut parts = vec!["https://music.apple.com"];
        parts.extend(country);
        parts.push(kind);
        parts.extend(name);
        parts.push(id);
        parts.join("/")
    };
    Some(match rest {
        ["album", name, id] => found.page(join("album", Some(name), id)),
        ["album", id] => found.page(join("album", None, id)),
        ["artist", name, id] => found.profile(join("artist", Some(name), id)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn covers() {
        file(
            &APPLE_MUSIC,
            "https://is1-ssl.mzstatic.com/image/thumb/Music113/v4/9e/22/c2/9e22c2fb-ef9c-b79b-7417-8bc714b85e51/4580547326338.jpg/296x296bb.webp",
            Some(
                "https://a1.mzstatic.com/us/r1000/0/Music113/v4/9e/22/c2/9e22c2fb-ef9c-b79b-7417-8bc714b85e51/4580547326338.jpg",
            ),
        );
        file(
            &APPLE_MUSIC,
            "https://is1-ssl.mzstatic.com/image/thumb/WNmTtyvl_DbiE8TEAhz_3A/9999x0w.png",
            Some("https://is1-ssl.mzstatic.com/image/thumb/WNmTtyvl_DbiE8TEAhz_3A/10000x10000.png"),
        );
    }

    #[test]
    fn albums_and_artists() {
        page(
            &APPLE_MUSIC,
            "https://itunes.apple.com/us/album/disasterpiece/id1870255337",
            "https://music.apple.com/us/album/disasterpiece/1870255337",
        );
        profile(
            &APPLE_MUSIC,
            "https://music.apple.com/us/artist/guchiry/1438071892",
            "https://music.apple.com/us/artist/guchiry/1438071892",
        );
    }
}
