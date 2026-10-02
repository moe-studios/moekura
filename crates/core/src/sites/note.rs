//! note: posts (`note.com/<user>/n/<id>`), creators, and images on
//! st-note.com.

use super::parts::Parts;
use super::{NOTE, SourceUrl};

const RESERVED: &[&str] = &[
    "hashtag", "intent", "login", "magazine", "signup", "terms", "topic", "users", "api", "n",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let cdn = matches!(
        p.host.as_str(),
        "d291vdycu0ht11.cloudfront.net" | "d2l930y2yx77uc.cloudfront.net"
    );
    if !matches!(p.domain.as_str(), "note.com" | "note.mu" | "st-note.com") && !cdn {
        return None;
    }
    let found = SourceUrl::of(&NOTE);
    let path = p.path();
    if cdn || p.domain == "st-note.com" {
        return Some(match path.as_slice() {
            ["img", file] => {
                found.file(format!("https://d2l930y2yx77uc.cloudfront.net/img/{file}"))
            }
            _ => found.file(None),
        });
    }
    let user = |name: &str| format!("https://note.com/{name}");
    Some(match path.as_slice() {
        [name, "n", id] if !RESERVED.contains(name) => found
            .page(format!("{}/n/{id}", user(name)))
            .profile(user(name)),
        [name, ..] if !RESERVED.contains(name) => found.profile(user(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_creators_and_images() {
        page(
            &NOTE,
            "https://note.mu/koma_labo/n/n32fb90fac512",
            "https://note.com/koma_labo/n/n32fb90fac512",
        );
        profile(
            &NOTE,
            "https://note.com/schizo_emu/m/m4d6814cea4e3",
            "https://note.com/schizo_emu",
        );
        file(
            &NOTE,
            "https://assets.st-note.com/img/1623726537463-B8LOZ1JZUS.png?width=2000",
            Some("https://d2l930y2yx77uc.cloudfront.net/img/1623726537463-B8LOZ1JZUS.png"),
        );
    }
}
