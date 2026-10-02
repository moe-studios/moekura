//! pixivFANBOX: posts (`<creator>.fanbox.cc/posts/<id>`,
//! `fanbox.cc/@<creator>/posts/<id>`, the old Pixiv paths), creators, and
//! files on `downloads.fanbox.cc`.

use super::parts::{Parts, is_digits};
use super::{FANBOX, SourceUrl};

const RESERVED: &[&str] = &["", "www", "downloads", "api"];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let path = p.path();
    let on_pixiv =
        matches!(p.domain.as_str(), "pixiv.net" | "pximg.net") && path.contains(&"fanbox");
    if p.domain != "fanbox.cc" && p.host != "fanbox.pixiv.net" && !on_pixiv {
        return None;
    }
    let found = SourceUrl::of(&FANBOX);
    let creator = |name: &str| format!("https://{name}.fanbox.cc");
    let old_creator = |id: &str| format!("https://www.pixiv.net/fanbox/creator/{id}");
    if matches!(
        p.host.as_str(),
        "downloads.fanbox.cc" | "fanbox.pixiv.net" | "pixiv.pximg.net"
    ) {
        // A sample (`/c/1200x630/`, `/w/1200/`) isn't the original, whose
        // extension can differ: the strategy finds it.
        let sample = path
            .windows(2)
            .any(|w| matches!(w[0], "c" | "w") && w[1].contains(|c: char| c.is_ascii_digit()));
        let found = found
            .file((!sample).then(|| p.without_query()))
            .sample(sample);
        return Some(match path.as_slice() {
            [.., "creator", id, _, _] if is_digits(id) => found.profile(old_creator(id)),
            _ => found,
        });
    }
    let sub = (!RESERVED.contains(&p.sub.as_str())).then_some(p.sub.as_str());
    Some(match (sub, path.as_slice()) {
        (None, [name, "posts", id]) if name.starts_with('@') => {
            let name = &name[1..];
            found
                .page(format!("{}/posts/{id}", creator(name)))
                .profile(creator(name))
        }
        (None, [name, ..]) if name.starts_with('@') => found.profile(creator(&name[1..])),
        (None, ["fanbox", "creator", user, "post", id]) => found
            .page(format!("{}/post/{id}", old_creator(user)))
            .profile(old_creator(user)),
        (None, ["fanbox", "creator" | "user", user, ..]) => found.profile(old_creator(user)),
        (None, ["fanbox", "member.php"]) => match p.param("user_id") {
            Some(user) => found.profile(old_creator(&user)),
            None => found,
        },
        (Some(name), ["posts", id]) => found
            .page(format!("{}/posts/{id}", creator(name)))
            .profile(creator(name)),
        (Some(name), _) => found.profile(creator(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_creators_and_files() {
        page(
            &FANBOX,
            "https://www.fanbox.cc/@tsukiori/posts/1080657",
            "https://tsukiori.fanbox.cc/posts/1080657",
        );
        page(
            &FANBOX,
            "https://omu001.fanbox.cc/posts/39714",
            "https://omu001.fanbox.cc/posts/39714",
        );
        page(
            &FANBOX,
            "https://www.pixiv.net/fanbox/creator/1566167/post/39714",
            "https://www.pixiv.net/fanbox/creator/1566167/post/39714",
        );
        profile(
            &FANBOX,
            "https://fanbox.cc/@shaggysusu/",
            "https://shaggysusu.fanbox.cc",
        );
        profile(
            &FANBOX,
            "https://omu001.fanbox.cc/posts",
            "https://omu001.fanbox.cc",
        );
        file(
            &FANBOX,
            "https://downloads.fanbox.cc/images/post/39714/JvjJal8v1yLgc5DPyEI05YpT.png",
            Some("https://downloads.fanbox.cc/images/post/39714/JvjJal8v1yLgc5DPyEI05YpT.png"),
        );
        file(
            &FANBOX,
            "https://downloads.fanbox.cc/images/post/39714/c/1200x630/JvjJal8v1yLgc5DPyEI05YpT.jpeg",
            None,
        );
    }
}
