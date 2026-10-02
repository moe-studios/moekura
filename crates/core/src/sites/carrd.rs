//! Carrd sites (`<name>.carrd.co`, `<name>.crd.co`): link pages, whose
//! sections can be galleries.

use super::parts::Parts;
use super::{CARRD, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "carrd.co" | "crd.co") {
        return None;
    }
    let found = SourceUrl::of(&CARRD);
    if matches!(p.sub.as_str(), "" | "www") {
        return Some(found);
    }
    let home = format!("https://{}.{}", p.sub, p.domain);
    Some(match p.path().as_slice() {
        ["assets", "images" | "videos", ..] => {
            let full = (p.ext().as_deref() == Some("mp4")
                || p.stem().is_some_and(|s| s.ends_with("_original")))
            .then(|| p.without_query());
            found.file(full).profile(home)
        }
        _ => match p
            .fragment()
            .filter(|f| f.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        {
            Some(section) => found.page(format!("{home}/#{section}")).profile(home),
            None => found.profile(home),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn sites_and_sections() {
        profile(
            &CARRD,
            "https://caminukai-art.carrd.co",
            "https://caminukai-art.carrd.co",
        );
        page(
            &CARRD,
            "https://caminukai-art.carrd.co/#fanart-shadowheartguidance",
            "https://caminukai-art.carrd.co/#fanart-shadowheartguidance",
        );
        profile(&CARRD, "https://otonokj.crd.co/", "https://otonokj.crd.co");
    }
}
