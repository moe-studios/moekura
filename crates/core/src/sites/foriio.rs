//! Foriio portfolios: works (`foriio.com/works/<id>`), users, and images
//! on imgix.

use super::parts::Parts;
use super::{FORIIO, SourceUrl};

const RESERVED: &[&str] = &[
    "about", "benefits", "business", "company", "contests", "discover", "keywords", "legal", "pro",
    "works", "embeded", "api",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let cdn = matches!(
        p.host.as_str(),
        "foriio.imgix.net" | "dyci7co52mbcc.cloudfront.net"
    ) || p.host.starts_with("foriio-og-");
    if !matches!(p.domain.as_str(), "foriio.com" | "fori.io") && !cdn {
        return None;
    }
    let found = SourceUrl::of(&FORIIO);
    let path = p.path();
    if cdn || p.sub == "imgx" {
        return Some(match path.as_slice() {
            [.., "store", file] => found.file(format!("https://foriio.imgix.net/store/{file}")),
            _ => found.file(None),
        });
    }
    Some(match path.as_slice() {
        ["works", id] | ["embeded", "works", id] => {
            found.page(format!("https://www.foriio.com/works/{id}"))
        }
        [name, ..] if !RESERVED.contains(name) => {
            found.profile(format!("https://www.foriio.com/{name}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn works_users_and_images() {
        page(
            &FORIIO,
            "https://www.foriio.com/embeded/works/600743",
            "https://www.foriio.com/works/600743",
        );
        profile(
            &FORIIO,
            "https://fori.io/comori22",
            "https://www.foriio.com/comori22",
        );
        file(
            &FORIIO,
            "https://foriio.imgix.net/store/46d77f4f772f191d04c9360180cc907d.jpg?w=2184",
            Some("https://foriio.imgix.net/store/46d77f4f772f191d04c9360180cc907d.jpg"),
        );
    }
}
