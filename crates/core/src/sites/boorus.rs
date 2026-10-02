//! Other boorus: Danbooru, e621, the Gelbooru family (Gelbooru,
//! Safebooru, TBIB, Rule34.xxx), Moebooru (Konachan, yande.re),
//! Rule34.us and Zerochan. Their posts, and files that name a post or its
//! MD5.

use super::parts::{Parts, is_digits, is_hex, leading_digits};
use super::{
    DANBOORU, E621, GELBOORU, KONACHAN, RULE34_US, RULE34_XXX, SAFEBOORU, Site, SourceUrl, TBIB,
    YANDERE, ZEROCHAN,
};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    match p.domain.as_str() {
        "donmai.us" | "donmai.moe" => Some(danbooru(p)),
        "e621.net" | "e926.net" => Some(e621(p)),
        "gelbooru.com" => Some(gelbooru(p, &GELBOORU)),
        "safebooru.org" => Some(gelbooru(p, &SAFEBOORU)),
        "tbib.org" => Some(gelbooru(p, &TBIB)),
        "rule34.xxx" => Some(gelbooru(p, &RULE34_XXX)),
        "yande.re" => Some(moebooru(p, &YANDERE)),
        "konachan.com" | "konachan.net" => Some(moebooru(p, &KONACHAN)),
        "rule34.us" => Some(rule34_us(p)),
        "zerochan.net" => Some(zerochan(p)),
        _ => None,
    }
}

/// The 32-hex-digit MD5 a file is named by, after an optional prefix
/// (`sample_`, `__tags__sample-`).
fn md5_in(name: &str) -> Option<&str> {
    let stem = name.split('.').next()?;
    let md5 = stem.get(stem.len().checked_sub(32)?..)?;
    is_hex(md5, 32).then_some(md5)
}

fn danbooru(p: &Parts) -> SourceUrl {
    let found = SourceUrl::of(&DANBOORU);
    let post = |id: &str| format!("https://danbooru.donmai.us/posts/{id}");
    let by_md5 = |md5: &str| format!("https://danbooru.donmai.us/posts?md5={md5}");
    match p.path().as_slice() {
        ["posts", id] => match leading_digits(id) {
            Some(id) => found.page(post(id)),
            None => found,
        },
        ["posts"] => match p.param("md5").filter(|m| is_hex(m, 32)) {
            Some(md5) => found.page(by_md5(&md5)),
            None => found,
        },
        ["users", id] => match leading_digits(id) {
            Some(id) => found.profile(format!("https://danbooru.donmai.us/users/{id}")),
            None => found,
        },
        [dirs @ .., file] if !dirs.is_empty() && md5_in(file).is_some() => {
            let md5 = md5_in(file).unwrap_or_default();
            let original = dirs[0] == "original" || is_hex(dirs[0], 2);
            let full = p.ext().filter(|_| original).map(|ext| {
                format!(
                    "https://cdn.donmai.us/original/{}/{}/{md5}.{ext}",
                    &md5[..2],
                    &md5[2..4]
                )
            });
            found.file(full).sample(!original).page(by_md5(md5))
        }
        _ => found,
    }
}

fn e621(p: &Parts) -> SourceUrl {
    let found = SourceUrl::of(&E621);
    let by_md5 = |md5: &str| format!("https://e621.net/posts?md5={md5}");
    match p.path().as_slice() {
        ["posts"] => match p.param("md5").filter(|m| is_hex(m, 32)) {
            Some(md5) => found.page(by_md5(&md5)),
            None => found,
        },
        ["posts", id] => match leading_digits(id) {
            Some(id) => found.page(format!("https://e621.net/posts/{id}")),
            None => found,
        },
        ["users", id] if is_digits(id) => found.profile(format!("https://e621.net/users/{id}")),
        ["data", a, b, file] if is_hex(a, 2) && is_hex(b, 2) => match md5_in(file) {
            Some(md5) => found.file(p.without_query()).page(by_md5(md5)),
            None => found.file(None),
        },
        ["data", _, a, b, file] if is_hex(a, 2) && is_hex(b, 2) => {
            // Samples: `<md5>.jpg`, `<md5>_720p.mp4`.
            let md5 = file.get(..32).filter(|m| is_hex(m, 32));
            let found = found.file(None).sample(true);
            match md5 {
                Some(md5) => found.page(by_md5(md5)),
                None => found,
            }
        }
        _ => found,
    }
}

fn gelbooru(p: &Parts, site: &'static Site) -> SourceUrl {
    let found = SourceUrl::of(site);
    let domain = &p.domain;
    let post = |id: &str| format!("https://{domain}/index.php?page=post&s=view&id={id}");
    let by_md5 = |md5: &str| format!("https://{domain}/index.php?page=post&s=list&md5={md5}");
    match p.path().as_slice() {
        ["index.php"] => {
            let (page, s) = (p.param("page"), p.param("s"));
            let id = p.param("id").filter(|id| is_digits(id));
            match (page.as_deref(), s.as_deref(), id, p.param("md5")) {
                (Some("post"), Some("view"), Some(id), _) | (Some("dapi"), _, Some(id), _) => {
                    found.page(post(&id))
                }
                (Some("post"), Some("list"), _, Some(md5)) if is_hex(&md5, 32) => {
                    found.page(by_md5(&md5))
                }
                _ => found,
            }
        }
        [kind @ ("images" | "samples" | "thumbnails"), .., file] => {
            let full = (*kind == "images").then(|| p.without_query());
            // `?<post id>` after some files names the post.
            let found = found.sample(*kind != "images");
            let found = match p.query().filter(|q| is_digits(q)) {
                Some(id) => found.page(post(id)),
                None => found,
            };
            match md5_in(file) {
                Some(md5) if found.page_url.is_none() => found.page(by_md5(md5)).file(full),
                _ => found.file(full),
            }
        }
        _ => found,
    }
}

fn moebooru(p: &Parts, site: &'static Site) -> SourceUrl {
    let found = SourceUrl::of(site);
    let domain = &p.domain;
    let post = |id: &str| format!("https://{domain}/post/show/{id}");
    let by_md5 = |md5: &str| format!("https://{domain}/post/show?md5={md5}");
    match p.path().as_slice() {
        ["post", "show", id, ..] if is_digits(id) => found.page(post(id)),
        ["post", "show"] => match p.param("md5").filter(|m| is_hex(m, 32)) {
            Some(md5) => found.page(by_md5(&md5)),
            None => found,
        },
        ["post"] => match p
            .param("tags")
            .and_then(|t| t.strip_prefix("md5:").map(str::to_owned))
            .filter(|m| is_hex(m, 32))
        {
            Some(md5) => found.page(by_md5(&md5)),
            None => found,
        },
        ["data", "preview", .., file] => match md5_in(file) {
            Some(md5) => found.file(None).sample(true).page(by_md5(md5)),
            None => found.file(None).sample(true),
        },
        [kind @ ("sample" | "jpeg" | "image"), md5, rest @ ..] => {
            let md5 = md5.get(..32).filter(|m| is_hex(m, 32));
            let Some(md5) = md5 else {
                return found.file(None);
            };
            // `yande.re 290757 tags….jpg`, `Konachan.com - 270803 ….jpg`.
            let id = rest.first().and_then(|name| {
                name.split([' ', '.'])
                    .find(|w| is_digits(w))
                    .map(str::to_owned)
            });
            let full = (*kind == "image").then(|| p.without_query());
            let found = found.file(full).sample(*kind != "image");
            match id {
                Some(id) => found.page(post(&id)),
                None => found.page(by_md5(md5)),
            }
        }
        _ => found,
    }
}

fn rule34_us(p: &Parts) -> SourceUrl {
    let found = SourceUrl::of(&RULE34_US);
    match p.path().as_slice() {
        ["index.php"] => match (p.param("r").as_deref(), p.param("id")) {
            (Some("posts/view"), Some(id)) if is_digits(&id) => {
                found.page(format!("https://rule34.us/index.php?r=posts/view&id={id}"))
            }
            _ => found,
        },
        ["hotlink.php"] => match p.param("hash").filter(|h| is_hex(h, 32)) {
            Some(md5) => found.page(format!("https://rule34.us/hotlink.php?hash={md5}")),
            None => found,
        },
        [kind @ ("images" | "thumbnails"), _, _, file] => {
            let found = found.file((*kind == "images").then(|| p.without_query()));
            match md5_in(file) {
                Some(md5) => found.page(format!("https://rule34.us/hotlink.php?hash={md5}")),
                None => found,
            }
        }
        _ => found,
    }
}

fn zerochan(p: &Parts) -> SourceUrl {
    let found = SourceUrl::of(&ZEROCHAN);
    let page = |id: &str| format!("https://www.zerochan.net/{id}#full");
    match p.path().as_slice() {
        // s4.zerochan.net/600/24/13/90674.jpg
        [size, a, b, file] if (*size == "full" || is_digits(size)) && p.has_file_ext() => {
            let id = p.stem().filter(|s| is_digits(s)).map(str::to_owned);
            let full = format!("https://static.zerochan.net/full/{a}/{b}/{file}");
            match id {
                Some(id) => found.file(full).page(page(&id)),
                None => found.file(full),
            }
        }
        // static.zerochan.net/Fullmetal.Alchemist.full.2831797.png
        [file] if p.has_file_ext() => {
            let pieces: Vec<&str> = file.rsplitn(4, '.').collect();
            match pieces.as_slice() {
                [ext, id, _size, title] if is_digits(id) => found
                    .file(format!(
                        "https://static.zerochan.net/{title}.full.{id}.{ext}"
                    ))
                    .page(page(id)),
                _ => found.file(None),
            }
        }
        ["full", id] | [id] if is_digits(id) => found.page(page(id)),
        _ => found,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, on, page, profile, sample};
    use super::*;

    #[test]
    fn danbooru_and_e621() {
        page(
            &DANBOORU,
            "https://danbooru.donmai.us/posts/1.json",
            "https://danbooru.donmai.us/posts/1",
        );
        profile(
            &DANBOORU,
            "https://danbooru.donmai.us/users/1",
            "https://danbooru.donmai.us/users/1",
        );
        file(
            &DANBOORU,
            "https://cdn.donmai.us/original/8d/81/8d819da4871c3ca39f428999df8220ce.jpg",
            Some("https://cdn.donmai.us/original/8d/81/8d819da4871c3ca39f428999df8220ce.jpg"),
        );
        let sample = on(
            &DANBOORU,
            "https://cdn.donmai.us/sample/8d/81/__sonetto_drawn_by_beishang_yutou__sample-8d819da4871c3ca39f428999df8220ce.jpg",
        );
        assert_eq!(
            sample.page_url.as_deref(),
            Some("https://danbooru.donmai.us/posts?md5=8d819da4871c3ca39f428999df8220ce")
        );
        page(
            &E621,
            "https://e621.net/posts/3728701",
            "https://e621.net/posts/3728701",
        );
        file(
            &E621,
            "https://static1.e621.net/data/6d/1a/6d1a6090ea82c2524212499797e7e53a.png",
            Some("https://static1.e621.net/data/6d/1a/6d1a6090ea82c2524212499797e7e53a.png"),
        );
    }

    #[test]
    fn samples() {
        sample(
            &DANBOORU,
            "https://cdn.donmai.us/original/8d/81/8d819da4871c3ca39f428999df8220ce.jpg",
            false,
        );
        sample(
            &DANBOORU,
            "https://cdn.donmai.us/sample/8d/81/__sonetto_drawn_by_beishang_yutou__sample-8d819da4871c3ca39f428999df8220ce.jpg",
            true,
        );
        sample(
            &SAFEBOORU,
            "https://safebooru.org//samples/4016/sample_64779fbfc87020ed5fd94854fe973bc0.jpg?4196692",
            true,
        );
    }

    #[test]
    fn gelbooru_family() {
        page(
            &GELBOORU,
            "https://www.gelbooru.com/index.php?page=post&s=view&id=7798045",
            "https://gelbooru.com/index.php?page=post&s=view&id=7798045",
        );
        page(
            &RULE34_XXX,
            "https://rule34.xxx/index.php?page=dapi&s=post&q=index&id=6961597&json=1",
            "https://rule34.xxx/index.php?page=post&s=view&id=6961597",
        );
        let found = on(
            &SAFEBOORU,
            "https://safebooru.org//samples/4016/sample_64779fbfc87020ed5fd94854fe973bc0.jpg?4196692",
        );
        assert!(found.is_file);
        assert_eq!(
            found.page_url.as_deref(),
            Some("https://safebooru.org/index.php?page=post&s=view&id=4196692")
        );
        file(
            &GELBOORU,
            "https://img2.gelbooru.com//images/a9/64/a96478bbf9bc3f0584f2b5ddf56025fa.webm",
            Some("https://img2.gelbooru.com//images/a9/64/a96478bbf9bc3f0584f2b5ddf56025fa.webm"),
        );
        on(
            &TBIB,
            "https://tbib.org/index.php?page=post&s=view&id=11509934",
        );
    }

    #[test]
    fn moebooru_rule34_us_and_zerochan() {
        page(
            &KONACHAN,
            "https://konachan.com/post/show/270803/banishment-bicycle",
            "https://konachan.com/post/show/270803",
        );
        let image = on(
            &YANDERE,
            "https://files.yande.re/image/2a5d1d688f565cb08a69ecf4e35017ab/yande.re%20349790%20breast_hold.jpg",
        );
        assert!(image.is_file);
        assert_eq!(
            image.page_url.as_deref(),
            Some("https://yande.re/post/show/349790")
        );
        page(
            &YANDERE,
            "https://yande.re/post?tags=md5:2c95b8975b73744da2bcbed9619c1d59",
            "https://yande.re/post/show?md5=2c95b8975b73744da2bcbed9619c1d59",
        );
        page(
            &RULE34_US,
            "https://rule34.us/index.php?r=posts/view&id=6204967",
            "https://rule34.us/index.php?r=posts/view&id=6204967",
        );
        page(
            &ZEROCHAN,
            "http://www.zerochan.net/1567893",
            "https://www.zerochan.net/1567893#full",
        );
        file(
            &ZEROCHAN,
            "https://s1.zerochan.net/Cocoa.Cookie.600.2957938.jpg",
            Some("https://static.zerochan.net/Cocoa.Cookie.full.2957938.jpg"),
        );
        file(
            &ZEROCHAN,
            "https://s4.zerochan.net/600/24/13/90674.jpg",
            Some("https://static.zerochan.net/full/24/13/90674.jpg"),
        );
    }
}
