//! Fandom wikis (`<wiki>.fandom.com`): file pages and images on
//! wikia.nocookie.net. A wiki is like an artist's profile.

use super::parts::Parts;
use super::{FANDOM, SourceUrl};

/// `ja`, `pt-br`: a language path prefix.
fn is_lang(s: &str) -> bool {
    let mut parts = s.split('-');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| (2..=3).contains(&p.len()) && p.chars().all(|c| c.is_ascii_lowercase()))
    };
    ok(parts.next()) && parts.next().is_none_or(|p| ok(Some(p))) && parts.next().is_none()
}

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "nocookie.net" | "fandom.com" | "wikia.com"
    ) {
        return None;
    }
    let found = SourceUrl::of(&FANDOM);
    let path = p.path();
    if p.domain == "nocookie.net" {
        return Some(match path.as_slice() {
            [uuid, ..] if super::parts::is_uuid(uuid) => found.file(format!(
                "https://static.wikia.nocookie.net/{uuid}?format=original"
            )),
            _ => {
                // <wiki>/[<lang>/]images/…: the wiki is the profile.
                let path: Vec<&str> = path
                    .iter()
                    .copied()
                    .skip_while(|s| s.starts_with("__cb"))
                    .collect();
                let images = path.iter().position(|s| *s == "images");
                let wiki = images.filter(|&at| at >= 1).map(|_| path[0]);
                let lang = images
                    .filter(|&at| at == 2 && is_lang(path[1]) && path[1] != "en")
                    .map(|_| path[1]);
                found.file(None).profile(wiki.map(|wiki| match lang {
                    Some(lang) => format!("https://{wiki}.fandom.com/{lang}"),
                    None => format!("https://{wiki}.fandom.com"),
                }))
            }
        });
    }
    if matches!(p.sub.as_str(), "" | "www" | "auth") {
        return Some(found);
    }
    let mut rest: &[&str] = &path;
    let lang = match rest {
        [lang, tail @ ..] if is_lang(lang) => {
            rest = tail;
            Some(*lang).filter(|l| *l != "en")
        }
        _ => None,
    };
    let wiki = match lang {
        Some(lang) => format!("https://{}.fandom.com/{lang}", p.sub),
        None => format!("https://{}.fandom.com", p.sub),
    };
    let rest = rest.strip_prefix(&["wiki"]).unwrap_or(rest);
    if rest.first() == Some(&"f") || rest.is_empty() {
        return Some(found.profile(wiki));
    }
    let page = rest.join("/");
    let file = page
        .strip_prefix("File:")
        .map(str::to_owned)
        .or_else(|| p.param("file"));
    let encode = |s: &str| {
        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
    };
    Some(match file {
        Some(file) if !page.starts_with("File:") => found
            .page(format!("{wiki}/wiki/{page}?file={}", encode(&file)))
            .profile(wiki),
        Some(file) => found
            .page(format!("{wiki}/wiki/File:{}", encode(&file)))
            .profile(wiki),
        None => found.page(format!("{wiki}/wiki/{page}")).profile(wiki),
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{on, page, profile};
    use super::*;

    #[test]
    fn wikis_pages_and_files() {
        page(
            &FANDOM,
            "https://typemoon.fandom.com/wiki/Tamamo-no-Mae?file=Caster_Extra_Takeuchi_design_1.png",
            "https://typemoon.fandom.com/wiki/Tamamo-no-Mae?file=Caster%5FExtra%5FTakeuchi%5Fdesign%5F1%2Epng",
        );
        page(
            &FANDOM,
            "https://typemoon.fandom.com/wiki/File:Aozaki.png",
            "https://typemoon.fandom.com/wiki/File:Aozaki%2Epng",
        );
        profile(
            &FANDOM,
            "https://genshin-impact.fandom.com/pt-br/f",
            "https://genshin-impact.fandom.com/pt-br",
        );
        let image = on(
            &FANDOM,
            "https://static.wikia.nocookie.net/valkyriecrusade/images/3/3f/Joan_Of_Arc.png/revision/latest/scale-to-width-down/270?cb=20170801081000",
        );
        assert!(image.is_file);
        assert_eq!(
            image.profile_url.as_deref(),
            Some("https://valkyriecrusade.fandom.com")
        );
    }
}
