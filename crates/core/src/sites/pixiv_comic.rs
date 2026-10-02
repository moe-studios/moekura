//! Pixiv Comic: magazines, works, stories (chapters) and novels on
//! `comic.pixiv.net`, and their images.

use super::parts::Parts;
use super::{PIXIV_COMIC, SourceUrl};

const HOSTS: &[&str] = &[
    "comic.pixiv.net",
    "img-comic.pximg.net",
    "public-img-comic.pximg.net",
    "img-novel.pximg.net",
    "public-img-novel.pximg.net",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !HOSTS.contains(&p.host.as_str()) {
        return None;
    }
    let found = SourceUrl::of(&PIXIV_COMIC);
    let page = |path: &str| format!("https://comic.pixiv.net/{path}");
    let path = p.path();
    if p.host != "comic.pixiv.net" {
        let at = path
            .iter()
            .position(|s| matches!(*s, "images" | "img-novel"));
        let rest = at.map_or(&path[..0], |at| &path[at..]);
        let base = p.basename().unwrap_or_default();
        return Some(match rest {
            ["images", "page", story, hash, _] => found
                .file(format!(
                    "https://img-comic.pximg.net/images/page/{story}/{hash}/{base}"
                ))
                .page(page(&format!("viewer/stories/{story}"))),
            ["images", "work_main" | "work_thumbnail", _] => {
                let work = p.stem().unwrap_or_default();
                found
                    .file(format!(
                        "https://public-img-comic.pximg.net/images/work_main/{base}"
                    ))
                    .page(page(&format!("works/{work}")))
            }
            ["images", "story_thumbnail", _, _] => found.file(None).page(page(&format!(
                "viewer/stories/{}",
                p.stem().unwrap_or_default()
            ))),
            [
                "images",
                kind @ ("magazine_cover" | "magazine_logo"),
                hash,
                _,
            ] => found
                .file(format!(
                    "https://public-img-comic.pximg.net/images/{kind}/{hash}/{base}"
                ))
                .page(page(&format!("magazines/{}", p.stem().unwrap_or_default()))),
            ["img-novel", "page", story, _, _] => found
                .file(p.as_str().to_owned())
                .page(page(&format!("novel/viewer/stories/{story}"))),
            _ => found.file(None),
        });
    }
    Some(match path.as_slice() {
        ["magazines", id] => found.page(page(&format!("magazines/{id}"))),
        ["works", id] => found.page(page(&format!("works/{id}"))),
        ["viewer", "stories", id] => found.page(page(&format!("viewer/stories/{id}"))),
        ["novel", "works", id] => found.page(page(&format!("novel/works/{id}"))),
        ["novel", "viewer", "stories", id] => {
            found.page(page(&format!("novel/viewer/stories/{id}")))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page};
    use super::*;

    #[test]
    fn works_stories_and_images() {
        page(
            &PIXIV_COMIC,
            "https://comic.pixiv.net/works/10137",
            "https://comic.pixiv.net/works/10137",
        );
        page(
            &PIXIV_COMIC,
            "https://comic.pixiv.net/viewer/stories/162153",
            "https://comic.pixiv.net/viewer/stories/162153",
        );
        file(
            &PIXIV_COMIC,
            "https://img-comic.pximg.net/c/q90_gridshuffle32:32/images/page/162153/iMnq837lBFlyCIpIstcp/1.jpg?20240112151247",
            Some("https://img-comic.pximg.net/images/page/162153/iMnq837lBFlyCIpIstcp/1.jpg"),
        );
        file(
            &PIXIV_COMIC,
            "https://public-img-comic.pximg.net/c!/w=200,f=webp%3Ajpeg/images/work_main/10137.jpg?20240217160416",
            Some("https://public-img-comic.pximg.net/images/work_main/10137.jpg"),
        );
    }
}
