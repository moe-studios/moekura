//! FC2 blogs (`<name>.blog<n>.fc2.com/blog-entry-<n>.html`), sites on
//! web.fc2.com and fc2web.com, and blog images.

use super::parts::{Parts, is_digits};
use super::{FC2, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "fc2.com" | "fc2web.com" | "fc2blog.net" | "fc2blog.us"
    ) {
        return None;
    }
    let found = SourceUrl::of(&FC2);
    let labels: Vec<&str> = p.sub.split('.').collect();
    let path = p.path();
    let is_blog = |s: &str| {
        s.strip_prefix("blog")
            .is_some_and(|n| n.is_empty() || is_digits(n))
    };
    match labels.as_slice() {
        // <name>.blog<n>.fc2.com
        [name, blog] if is_blog(blog) => {
            let home = format!("http://{name}.blog.{}", p.domain);
            let entry = match path.as_slice() {
                [file] => file
                    .strip_prefix("blog-entry-")
                    .and_then(|f| f.strip_suffix(".html"))
                    .map(str::to_owned),
                [] => p.param("no").filter(|n| is_digits(n)),
                _ => None,
            };
            let album = match path.as_slice() {
                ["img", file] => Some((*file).to_owned()),
                [] if p.param("mode").as_deref() == Some("image") => p.param("filename"),
                _ => None,
            };
            let found = found.profile(home.clone());
            Some(match (entry, album) {
                (Some(n), _) => found.page(format!("{home}/blog-entry-{n}.html")),
                (_, Some(file)) => found.page(format!("{home}/img/{file}/")),
                _ if p.has_file_ext() => found.file(None),
                _ => found,
            })
        }
        // blog-imgs-119.fc2.com/n/i/y/niyamalog/x.jpg
        [imgs] if imgs.starts_with("blog-imgs-") || *imgs == "blog" => {
            let found = found.file(None);
            Some(match path.as_slice() {
                [_, _, _, name, _] => found.profile(format!("http://{name}.blog.{}", p.domain)),
                _ => found,
            })
        }
        [_, "bbs" | "web" | "h" | "x"] if p.domain == "fc2.com" => {
            let home = format!("http://{}", p.host);
            Some(if p.has_file_ext() {
                found.file(None).profile(home)
            } else {
                found.profile(home)
            })
        }
        [_name] if p.domain == "fc2web.com" => Some(found.profile(format!("http://{}", p.host))),
        _ => Some(found),
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn blogs_and_sites() {
        page(
            &FC2,
            "http://hosystem.blog36.fc2.com/blog-entry-37.html",
            "http://hosystem.blog.fc2.com/blog-entry-37.html",
        );
        page(
            &FC2,
            "http://abk00.blog71.fc2.com/?no=3052",
            "http://abk00.blog.fc2.com/blog-entry-3052.html",
        );
        profile(
            &FC2,
            "http://silencexs.blog106.fc2.com",
            "http://silencexs.blog.fc2.com",
        );
        profile(
            &FC2,
            "http://794ancientkyoto.web.fc2.com",
            "http://794ancientkyoto.web.fc2.com",
        );
    }
}
