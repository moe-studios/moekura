//! Tumblr: posts (`<blog>.tumblr.com/post/<id>`, `tumblr.com/<blog>/<id>`),
//! blogs, and media (`s640x960` samples of the largest size).

use super::parts::{Parts, is_digits, is_hex};
use super::{SourceUrl, TUMBLR};

const RESERVED: &[&str] = &[
    "about",
    "app",
    "blog",
    "dashboard",
    "developers",
    "explore",
    "jobs",
    "login",
    "logo",
    "policy",
    "press",
    "register",
    "security",
    "tagged",
    "tips",
    "search",
    "settings",
    "likes",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "tumblr.com" | "tmblr.co") {
        return None;
    }
    let found = SourceUrl::of(&TUMBLR);
    if p.domain == "tmblr.co" {
        return Some(found);
    }
    let path = p.path();
    let media = p.sub.ends_with(".media")
        || matches!(
            p.sub.as_str(),
            "data" | "media" | "static" | "va.media" | "64.media"
        );
    if media {
        return Some(match path.as_slice() {
            [a, b, size, _] if is_hex(a, 32) && size.starts_with('s') && b.contains('-') => {
                let ext = match p.ext().as_deref() {
                    Some("pnj") => "png".to_owned(),
                    Some(ext) => ext.to_owned(),
                    None => String::new(),
                };
                let stem = p.stem().unwrap_or_default();
                found.file(format!(
                    "https://{}/{a}/{b}/s21000x21000/{stem}.{ext}",
                    p.host
                ))
            }
            _ => found.file(None),
        });
    }
    let blog = |name: &str| format!("https://{name}.tumblr.com");
    let post = |name: &str, id: &str| format!("{}/post/{id}", blog(name));
    let www = matches!(p.sub.as_str(), "" | "www" | "at");
    Some(match (www, path.as_slice()) {
        (false, ["post" | "image", id, ..]) if is_digits(id) => {
            found.page(post(&p.sub, id)).profile(blog(&p.sub))
        }
        (false, _) => found.profile(blog(&p.sub)),
        (true, ["blog", "view", id, ..]) if id.starts_with("t:") => {
            found.profile(format!("https://www.tumblr.com/blog/view/{id}"))
        }
        (true, ["blog", "view", name, id]) if is_digits(id) => {
            found.page(post(name, id)).profile(blog(name))
        }
        (true, ["blog", "view", name] | ["blog", name] | ["dashboard", "blog", name]) => {
            found.profile(blog(name))
        }
        (true, [name, id, ..]) if is_digits(id) && !RESERVED.contains(name) => {
            found.page(post(name, id)).profile(blog(name))
        }
        (true, [name, ..]) if !RESERVED.contains(name) => found.profile(blog(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_blogs_and_media() {
        page(
            &TUMBLR,
            "https://marmaladica.tumblr.com/post/188237914346/saved",
            "https://marmaladica.tumblr.com/post/188237914346",
        );
        page(
            &TUMBLR,
            "https://www.tumblr.com/yamujiburo/682910938493599744/will-tumblr-let-me-keep-this-up",
            "https://yamujiburo.tumblr.com/post/682910938493599744",
        );
        profile(
            &TUMBLR,
            "https://www.tumblr.com/dashboard/blog/dankwartart",
            "https://dankwartart.tumblr.com",
        );
        profile(
            &TUMBLR,
            "https://rosarrie.tumblr.com/archive",
            "https://rosarrie.tumblr.com",
        );
        file(
            &TUMBLR,
            "https://66.media.tumblr.com/168dabd09d5ad69eb5fedcf94c45c31a/3dbfaec9b9e0c2e3-72/s640x960/bf33a1324f3f36d2dc64f011bfeab4867da62bc8.pnj",
            Some(
                "https://66.media.tumblr.com/168dabd09d5ad69eb5fedcf94c45c31a/3dbfaec9b9e0c2e3-72/s21000x21000/bf33a1324f3f36d2dc64f011bfeab4867da62bc8.png",
            ),
        );
    }
}
