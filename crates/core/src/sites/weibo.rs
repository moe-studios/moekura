//! Weibo: posts (`weibo.com/<user>/<base 62 id>`, `m.weibo.cn/detail/<id>`),
//! users, and images on sinaimg.cn (`/mw690/` samples of `/large/`).

use super::parts::{Parts, is_digits};
use super::{SourceUrl, WEIBO};

const RESERVED: &[&str] = &[
    "u", "n", "p", "profile", "sinaurl", "status", "detail", "tv", "ajax", "login",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "weibo.com" | "weibo.cn" | "sinaimg.cn" | "weibocdn.com" | "t.cn"
    ) {
        return None;
    }
    let found = SourceUrl::of(&WEIBO);
    let path = p.path();
    if matches!(p.domain.as_str(), "sinaimg.cn" | "weibocdn.com") {
        if p.ext().as_deref() == Some("mp4") {
            return Some(found.file(p.as_str().to_owned()));
        }
        return Some(match path.as_slice() {
            [_, file] if p.domain == "sinaimg.cn" => {
                found.file(format!("https://{}/large/{file}", p.host))
            }
            _ => found.file(None),
        });
    }
    if p.domain == "t.cn" {
        return Some(found);
    }
    let base = "https://www.weibo.com";
    let user = |id: &str| format!("{base}/u/{id}");
    let detail = |id: &str| format!("https://m.weibo.cn/detail/{id}");
    Some(match (p.sub.as_str(), path.as_slice()) {
        ("tw", [_, id]) if is_digits(id) => found.page(detail(id)),
        ("photo", [owner, _, _, _, id, ..]) if is_digits(owner) && is_digits(id) => {
            found.page(detail(id)).profile(user(owner))
        }
        ("share.api", ["share", ids]) => {
            match ids.split(',').nth(1).map(|i| i.trim_end_matches(".html")) {
                Some(id) if is_digits(id) => found.page(detail(id)),
                _ => found,
            }
        }
        (_, ["detail", id]) if is_digits(id) => found.page(detail(id)),
        ("m", ["status", id]) => found.page(format!("https://m.weibo.cn/status/{id}")),
        (_, ["u" | "profile", id, ..]) if is_digits(id) => found.profile(user(id)),
        (_, ["p", id, ..]) if is_digits(id) => found.profile(format!("{base}/p/{id}")),
        (_, ["n", name, ..]) => found.profile(format!("{base}/n/{name}")),
        (_, [owner, id]) if is_digits(owner) && id.chars().all(|c| c.is_ascii_alphanumeric()) => {
            found
                .page(format!("{base}/{owner}/{id}"))
                .profile(user(owner))
        }
        (_, [owner, ..]) if is_digits(owner) => found.profile(user(owner)),
        (_, [name, ..])
            if !RESERVED.contains(name)
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
        {
            found.profile(format!("{base}/{name}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_users_and_images() {
        page(
            &WEIBO,
            "https://www.weibo.com/5501756072/IF9fugHzj?from=page_1005055501756072_profile",
            "https://www.weibo.com/5501756072/IF9fugHzj",
        );
        page(
            &WEIBO,
            "https://www.weibo.com/detail/4676597657371957",
            "https://m.weibo.cn/detail/4676597657371957",
        );
        page(
            &WEIBO,
            "https://share.api.weibo.cn/share/304950356,4767694689143828.html",
            "https://m.weibo.cn/detail/4767694689143828",
        );
        profile(
            &WEIBO,
            "https://www.weibo.com/u/5957640693/home?wvr=5",
            "https://www.weibo.com/u/5957640693",
        );
        profile(
            &WEIBO,
            "https://www.weibo.com/endlessnsmt",
            "https://www.weibo.com/endlessnsmt",
        );
        file(
            &WEIBO,
            "http://ww1.sinaimg.cn/mw690/69917555gw1f6ggdghk28j20c87lbhdt.jpg",
            Some("https://ww1.sinaimg.cn/large/69917555gw1f6ggdghk28j20c87lbhdt.jpg"),
        );
    }
}
