//! Xfolio portfolios: works (`xfolio.jp/portfolio/<user>/works/<id>`),
//! users, and images.

use super::parts::Parts;
use super::{SourceUrl, XFOLIO};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "xfolio.jp" {
        return None;
    }
    let found = SourceUrl::of(&XFOLIO);
    let user = |name: &str| format!("https://xfolio.jp/portfolio/{name}");
    let path = p.path();
    // An optional language comes first.
    let path = match path.as_slice() {
        [lang, rest @ ..] if lang.len() == 2 && rest.first() == Some(&"portfolio") => rest,
        all => all,
    };
    Some(match path {
        ["fullscale_image"] | ["user_asset.php"] => found.file(p.as_str().to_owned()),
        ["portfolio", name, "works", id] => found
            .page(format!("{}/works/{id}", user(name)))
            .profile(user(name)),
        ["portfolio", name, ..] => found.profile(user(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn works_and_users() {
        page(
            &XFOLIO,
            "https://xfolio.jp/en/portfolio/ben1shoga/works/237599",
            "https://xfolio.jp/portfolio/ben1shoga/works/237599",
        );
        profile(
            &XFOLIO,
            "https://xfolio.jp/portfolio/ben1shoga/works",
            "https://xfolio.jp/portfolio/ben1shoga",
        );
    }
}
