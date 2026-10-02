//! Blogger: posts on `<blog>.blogspot.com`, blogger.com profiles, and
//! images on blogger.googleusercontent.com and `*.bp.blogspot.com`
//! (`/s1600/` samples of `/d/` originals).

use super::parts::{Parts, is_digits};
use super::{BLOGGER, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    // blogspot.com.es and other country variants.
    let blogspot = p.domain == "blogspot.com" || p.host.contains(".blogspot.");
    if p.domain != "blogger.com" && !blogspot && p.host != "blogger.googleusercontent.com" {
        return None;
    }
    let found = SourceUrl::of(&BLOGGER);
    let path = p.path();
    if p.host == "blogger.googleusercontent.com" {
        return Some(match path.as_slice() {
            ["img", "b", dir, id, _, file, ..] => found.file(format!(
                "https://blogger.googleusercontent.com/img/b/{dir}/{id}/d/{file}"
            )),
            ["img", "a", id] => {
                let id = id.split('=').next().unwrap_or(id);
                found.file(format!(
                    "https://blogger.googleusercontent.com/img/a/{id}=d"
                ))
            }
            _ => found.file(None),
        });
    }
    let bp = (p.domain == "blogspot.com" && p.sub.ends_with(".bp"))
        || (p.domain == "blogger.com" && p.sub.starts_with("bp"));
    if bp {
        return Some(match path.as_slice() {
            [a, b, c, d, _, file, ..] => found.file(format!(
                "https://1.bp.blogspot.com/{a}/{b}/{c}/{d}/d/{file}"
            )),
            _ => found.file(None),
        });
    }
    if p.domain == "blogger.com" {
        return Some(match path.as_slice() {
            ["profile", id] => found.profile(format!("https://www.blogger.com/profile/{id}")),
            _ => found,
        });
    }
    // <blog>.blogspot.com, or a country variant (`x.blogspot.com.es`).
    let blog = p.host.split(".blogspot.").next().unwrap_or_default();
    if blog.is_empty() || blog.contains('.') || blog == "www" {
        return Some(found);
    }
    let home = format!("https://{blog}.blogspot.com");
    Some(match path.as_slice() {
        [year, month, post] if is_digits(year) && is_digits(month) && post.ends_with(".html") => {
            found
                .page(format!("{home}/{year}/{month}/{post}"))
                .profile(home)
        }
        ["p", page] if page.ends_with(".html") => {
            found.page(format!("{home}/p/{page}")).profile(home)
        }
        _ => found.profile(home),
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_blogs_and_images() {
        page(
            &BLOGGER,
            "http://vincentmcart.blogspot.com.es/2016/05/poison-sting.html?zx=141d0a1a4c3e3ba",
            "https://vincentmcart.blogspot.com/2016/05/poison-sting.html",
        );
        profile(
            &BLOGGER,
            "http://benbotport.blogspot.com",
            "https://benbotport.blogspot.com",
        );
        profile(
            &BLOGGER,
            "https://www.blogger.com/profile/05678559930985966952",
            "https://www.blogger.com/profile/05678559930985966952",
        );
        file(
            &BLOGGER,
            "https://4.bp.blogspot.com/-1ndmEdQX3AM/Tv04FWJ3kTI/AAAAAAAAAzg/P-WNaJRST6Q/s400/Bookworm%2B3.jpg",
            Some(
                "https://1.bp.blogspot.com/-1ndmEdQX3AM/Tv04FWJ3kTI/AAAAAAAAAzg/P-WNaJRST6Q/d/Bookworm+3.jpg",
            ),
        );
    }
}
