//! Ci-En: articles (`ci-en.net/creator/<id>/article/<id>`), creators, and
//! media on media.ci-en.jp.

use super::parts::Parts;
use super::{CI_EN, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let site = matches!(
        p.host.as_str(),
        "ci-en.jp" | "ci-en.net" | "ci-en.dlsite.com"
    );
    if !site && p.host != "media.ci-en.jp" {
        return None;
    }
    let found = SourceUrl::of(&CI_EN);
    let creator = |id: &str| {
        let id = id.trim_start_matches('0');
        format!("https://ci-en.net/creator/{id}")
    };
    let path = p.path();
    if !site {
        return Some(match path.as_slice() {
            [_, _, "creator", id, _, rest @ ..] => {
                // Only `/upload/` files are originals.
                let original = rest.first() == Some(&"upload");
                let full = original.then(|| p.as_str().to_owned());
                found.file(full).sample(!original).profile(creator(id))
            }
            _ => found.file(None),
        });
    }
    Some(match path.as_slice() {
        ["creator", id, "article", article, ..] => found
            .page(format!("{}/article/{article}", creator(id)))
            .profile(creator(id)),
        ["creator", id, ..] => found.profile(creator(id)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn articles_and_creators() {
        page(
            &CI_EN,
            "https://ci-en.dlsite.com/creator/5290/article/998146",
            "https://ci-en.net/creator/5290/article/998146",
        );
        profile(
            &CI_EN,
            "https://ci-en.net/creator/11019/article",
            "https://ci-en.net/creator/11019",
        );
    }
}
