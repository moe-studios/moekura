//! Miyoushe (miHoYo's forums) and HoYoLAB: articles, users' post lists,
//! and uploads.

use super::parts::Parts;
use super::{MIYOUSHE, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "mihoyo.com" | "miyoushe.com" | "hoyolab.com" | "hoyo.link"
    ) {
        return None;
    }
    let found = SourceUrl::of(&MIYOUSHE);
    let hoyolab = matches!(p.domain.as_str(), "hoyolab.com" | "hoyo.link");
    let base = |sub: Option<&str>| {
        if hoyolab {
            "https://www.hoyolab.com".to_owned()
        } else {
            format!("https://www.miyoushe.com/{}", sub.unwrap_or("sr"))
        }
    };
    let user = |id: &str| format!("{}/accountCenter/postList?id={id}", base(Some("sr")));
    if matches!(p.sub.as_str(), "upload-bbs" | "upload-os-bbs") {
        let full = p
            .without_query()
            .replace("upload-bbs.miyoushe.com", "upload-bbs.mihoyo.com");
        return Some(found.file(full));
    }
    if p.sub == "prod-vod-sign" {
        return Some(found.file(p.as_str().to_owned()));
    }
    let path = p.path();
    // Mobile pages keep the path in the fragment: `/bh3?…#/article/1`.
    let fragment: Vec<&str> = p
        .fragment()
        .filter(|f| f.starts_with('/'))
        .map(|f| {
            f.split('?')
                .next()
                .unwrap_or(f)
                .split('/')
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let article = |sub: Option<&str>, id: &str| format!("{}/article/{id}", base(sub));
    Some(match path.as_slice() {
        [sub, "article", id] if !hoyolab => found.page(article(Some(sub), id)),
        ["article", id] | ["genshin", "article", id] if hoyolab => found.page(article(None, id)),
        [.., "accountCenter", _] | [.., "accountCenter"] => match p.param("id") {
            Some(id) => found.profile(user(&id)),
            None => found,
        },
        rest => match (rest, fragment.as_slice()) {
            ([sub], ["article", id]) => found.page(article(Some(sub), id)),
            ([], ["article", id]) => found.page(article(None, id)),
            _ => found,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn articles_users_and_uploads() {
        page(
            &MIYOUSHE,
            "https://bbs.mihoyo.com/bh3/article/28939887",
            "https://www.miyoushe.com/bh3/article/28939887",
        );
        page(
            &MIYOUSHE,
            "https://www.hoyolab.com/genshin/article/196109",
            "https://www.hoyolab.com/article/196109",
        );
        page(
            &MIYOUSHE,
            "https://m.miyoushe.com/bh3?channel=miyousheluodi%2F#/article/27266673",
            "https://www.miyoushe.com/bh3/article/27266673",
        );
        profile(
            &MIYOUSHE,
            "https://www.miyoushe.com/bh3/accountCenter/postList?id=73731802",
            "https://www.miyoushe.com/sr/accountCenter/postList?id=73731802",
        );
        file(
            &MIYOUSHE,
            "https://upload-bbs.miyoushe.com/upload/2022/09/14/73731802/2e25565bd6fa86d86b581e151e9778ac_8107601733815763725.jpg?x-oss-process=image/resize,s_600",
            Some(
                "https://upload-bbs.mihoyo.com/upload/2022/09/14/73731802/2e25565bd6fa86d86b581e151e9778ac_8107601733815763725.jpg",
            ),
        );
    }
}
