//! Minitokyo: gallery entries (`gallery.minitokyo.net/view/<id>`), users,
//! and images (`/view/` samples of `/downloads/` originals).

use super::parts::Parts;
use super::{MINITOKYO, SourceUrl};

const RESERVED: &[&str] = &["", "forum", "gallery", "my", "static", "www"];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "minitokyo.net" {
        return None;
    }
    let found = SourceUrl::of(&MINITOKYO);
    let page = |id: &str| format!("http://gallery.minitokyo.net/view/{id}");
    let path = p.path();
    if p.sub.starts_with("static") {
        return Some(match path.as_slice() {
            ["downloads" | "view" | "thumbs", a, b, file] => {
                let id = p
                    .stem()
                    .map(|s| s.split('-').next().unwrap_or(s).to_owned());
                found
                    .file(format!(
                        "http://static.minitokyo.net/downloads/{a}/{b}/{file}"
                    ))
                    .page(id.map(|id| page(&id)))
            }
            _ => found.file(None),
        });
    }
    Some(match (p.sub.as_str(), path.as_slice()) {
        ("gallery", ["view" | "download", id, ..]) => found.page(page(id)),
        (name, _) if !RESERVED.contains(&name) => {
            found.profile(format!("http://{name}.minitokyo.net"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn entries_users_and_images() {
        page(
            &MINITOKYO,
            "http://gallery.minitokyo.net/download/571342/1",
            "http://gallery.minitokyo.net/view/571342",
        );
        profile(
            &MINITOKYO,
            "http://deto15.minitokyo.net",
            "http://deto15.minitokyo.net",
        );
        file(
            &MINITOKYO,
            "http://static2.minitokyo.net/view/39/41/332089.jpg",
            Some("http://static.minitokyo.net/downloads/39/41/332089.jpg"),
        );
    }
}
