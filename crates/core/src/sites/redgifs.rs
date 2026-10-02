//! RedGIFs: GIFs (`redgifs.com/watch/<id>`), users, and media.

use super::parts::Parts;
use super::{REDGIFS, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "redgifs.com" {
        return None;
    }
    let found = SourceUrl::of(&REDGIFS);
    let watch = |id: &str| format!("https://www.redgifs.com/watch/{}", id.to_ascii_lowercase());
    let gif_of = |stem: &str| stem.split('-').next().unwrap_or(stem).to_owned();
    let path = p.path();
    Some(match (p.sub.as_str(), path.as_slice()) {
        (sub, [_]) if sub.starts_with("thumbs") || sub == "media" => {
            let gif = gif_of(p.stem().unwrap_or_default());
            found.file(p.as_str().to_owned()).page(watch(&gif))
        }
        ("userpic", _) => found.file(None),
        ("i", ["i", _]) => found.page(watch(p.stem().unwrap_or_default())),
        ("api", ["v2", "gifs", id, ..]) => found.page(watch(id)),
        (_, ["watch" | "ifr", id]) => found.page(watch(id)),
        (_, ["users", name, ..]) => found.profile(format!(
            "https://www.redgifs.com/users/{}",
            name.to_ascii_lowercase()
        )),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn gifs_and_users() {
        page(
            &REDGIFS,
            "https://www.redgifs.com/ifr/thunderousverifiablescoter",
            "https://www.redgifs.com/watch/thunderousverifiablescoter",
        );
        page(
            &REDGIFS,
            "https://api.redgifs.com/v2/gifs/thunderousverifiablescoter?views=yes",
            "https://www.redgifs.com/watch/thunderousverifiablescoter",
        );
        profile(
            &REDGIFS,
            "https://redgifs.com/users/LazyProcrastinator/collections",
            "https://www.redgifs.com/users/lazyprocrastinator",
        );
    }
}
