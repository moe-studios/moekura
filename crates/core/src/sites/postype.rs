//! Postype: posts (`postype.com/@<blog>/post/<id>`, `<blog>.postype.com/post/<id>`),
//! series, blogs, and images on CloudFront.

use super::parts::{Parts, is_digits};
use super::{POSTYPE, SourceUrl};

const RESERVED: &[&str] = &["", "about", "blog", "c3", "i", "www"];
const CDNS: &[&str] = &[
    "d2ufj6gm1gtdrc.cloudfront.net",
    "d3mcojo3jv0dbr.cloudfront.net",
    "d33pksfia2a94m.cloudfront.net",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let cdn = CDNS.contains(&p.host.as_str());
    if !matches!(p.domain.as_str(), "postype.com" | "posty.pe") && !cdn {
        return None;
    }
    let found = SourceUrl::of(&POSTYPE);
    if cdn {
        return Some(found.file(p.without_query()));
    }
    if p.domain == "posty.pe" {
        return Some(found);
    }
    let blog = |name: &str| format!("https://www.postype.com/@{name}");
    let mut path = p.path();
    if path.first().is_some_and(|s| matches!(*s, "en" | "ko")) {
        path.remove(0);
    }
    let sub = (!RESERVED.contains(&p.sub.as_str())).then_some(p.sub.as_str());
    let at = |s: &str| s.strip_prefix('@').map(str::to_owned);
    Some(match (sub, path.as_slice()) {
        (_, ["_next", "image"]) => found.file(
            p.param("url")
                .and_then(|u| super::parse(&u))
                .and_then(|u| u.file_url),
        ),
        (Some(name), ["post", id]) if is_digits(id) => found
            .page(format!("{}/post/{id}", blog(name)))
            .profile(blog(name)),
        (None, [name, "post", id]) if at(name).is_some() && is_digits(id) => {
            let name = at(name).unwrap_or_default();
            found
                .page(format!("{}/post/{id}", blog(&name)))
                .profile(blog(&name))
        }
        (Some(name), ["series", id, ..]) => found
            .page(format!("{}/series/{id}", blog(name)))
            .profile(blog(name)),
        (None, [name, "series", id]) if at(name).is_some() => {
            let name = at(name).unwrap_or_default();
            found
                .page(format!("{}/series/{id}", blog(&name)))
                .profile(blog(&name))
        }
        (None, ["profile", name, ..]) => found.profile(format!(
            "https://www.postype.com/profile/@{}",
            name.trim_start_matches('@')
        )),
        (None, [name, ..]) if at(name).is_some() => {
            found.profile(blog(&at(name).unwrap_or_default()))
        }
        (Some(name), _) => found.profile(blog(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_blogs_and_images() {
        page(
            &POSTYPE,
            "https://luland.postype.com/post/11659399",
            "https://www.postype.com/@luland/post/11659399",
        );
        page(
            &POSTYPE,
            "https://www.postype.com/en/@fruitsnoir/post/5316533",
            "https://www.postype.com/@fruitsnoir/post/5316533",
        );
        profile(
            &POSTYPE,
            "https://luland.postype.com/posts",
            "https://www.postype.com/@luland",
        );
        profile(
            &POSTYPE,
            "https://www.postype.com/profile/@ep58bc/posts",
            "https://www.postype.com/profile/@ep58bc",
        );
        file(
            &POSTYPE,
            "https://d2ufj6gm1gtdrc.cloudfront.net/2018/09/10/22/49/e91aea7d82404cdfcb12ecbc99ef856f.jpg?w=1200&q=90",
            Some(
                "https://d2ufj6gm1gtdrc.cloudfront.net/2018/09/10/22/49/e91aea7d82404cdfcb12ecbc99ef856f.jpg",
            ),
        );
    }
}
