//! Lofter: posts (`<user>.lofter.com/post/<id>`), blogs, and images on
//! lf127.net.

use super::parts::Parts;
use super::{LOFTER, SourceUrl};

const RESERVED: &[&str] = &["", "i", "uls", "www"];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "lofter.com" | "127.net" | "lf127.net" | "126.net"
    ) {
        return None;
    }
    let found = SourceUrl::of(&LOFTER);
    if p.domain != "lofter.com" {
        return Some(found.file(if p.domain == "126.net" {
            p.as_str().to_owned()
        } else {
            p.without_query()
        }));
    }
    let blog = |name: &str| format!("https://{name}.lofter.com");
    let path = p.path();
    if p.sub == "uls" {
        // A share link wrapping the post's.
        return Some(match p.param("h5url").and_then(|u| super::parse(&u)) {
            Some(inner) if inner.site.key == "lofter" => inner,
            _ => found,
        });
    }
    if !RESERVED.contains(&p.sub.as_str()) {
        let name = p.sub.as_str();
        return Some(match path.as_slice() {
            [.., "post", id] => found
                .page(format!("{}/post/{id}", blog(name)))
                .profile(blog(name)),
            _ => found.profile(blog(name)),
        });
    }
    Some(match path.as_slice() {
        ["front", "blog", "home-page", name] | ["app" | "blog", name] => found.profile(blog(name)),
        ["mentionredirect.do"] => match p.param("blogId") {
            Some(id) => found.profile(format!(
                "https://www.lofter.com/mentionredirect.do?blogId={id}"
            )),
            None => found,
        },
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
            &LOFTER,
            "https://gengar563.lofter.com/front/post/1e82da8c_1c98dae1b",
            "https://gengar563.lofter.com/post/1e82da8c_1c98dae1b",
        );
        page(
            &LOFTER,
            "https://uls.lofter.com/?h5url=https%3A%2F%2Flesegeng.lofter.com%2Fpost%2F1f0aec07_2bbc5ce0b",
            "https://lesegeng.lofter.com/post/1f0aec07_2bbc5ce0b",
        );
        profile(
            &LOFTER,
            "http://www.lofter.com/app/xiaokonggedmx",
            "https://xiaokonggedmx.lofter.com",
        );
        file(
            &LOFTER,
            "https://imglf3.lf127.net/img/S1d2QlVsWkJhSW1qcnpIS0ZSa3ZJSzFCWFlnUWgzb01DcUdpT1lreG5yQjJVMkhGS09HNGR3PT0.png?imageView&thumbnail=1680x0",
            Some(
                "https://imglf3.lf127.net/img/S1d2QlVsWkJhSW1qcnpIS0ZSa3ZJSzFCWFlnUWgzb01DcUdpT1lreG5yQjJVMkhGS09HNGR3PT0.png",
            ),
        );
    }
}
