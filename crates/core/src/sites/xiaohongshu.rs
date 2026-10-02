//! Xiaohongshu (RedNote): notes (`xiaohongshu.com/explore/<id>`), users,
//! and images on xhscdn.com.

use super::parts::{Parts, is_hex};
use super::{SourceUrl, XIAOHONGSHU};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "xiaohongshu.com"
            | "rednote.com"
            | "xhscdn.com"
            | "rednotecdn.com"
            | "xhslink.com"
            | "xhslink.cn"
    ) {
        return None;
    }
    let found = SourceUrl::of(&XIAOHONGSHU);
    let path = p.path();
    match p.domain.as_str() {
        "xhscdn.com" | "rednotecdn.com" => {
            return Some(match path.as_slice() {
                [stamp, hash, rest @ ..]
                    if stamp.len() == 12 && is_hex(hash, 32) && !rest.is_empty() =>
                {
                    let mut parts: Vec<String> = rest.iter().map(|s| (*s).to_owned()).collect();
                    if let Some(last) = parts.last_mut() {
                        *last = last.split('!').next().unwrap_or_default().to_owned();
                    }
                    found.file(format!("https://ci.xiaohongshu.com/{}", parts.join("/")))
                }
                _ => found.file(None),
            });
        }
        "xhslink.com" | "xhslink.cn" => return Some(found),
        _ => {}
    }
    if p.sub == "ci" {
        return Some(found.file(p.without_query()));
    }
    if p.sub == "img" {
        return Some(found.file(p.without_query().split('@').next().map(str::to_owned)));
    }
    let base = "https://www.xiaohongshu.com";
    let user = |id: &str| format!("{base}/user/profile/{id}");
    // The token is needed to see the note.
    let token = |page: String| match p.param("xsec_token") {
        Some(token) => format!("{page}?xsec_token={token}"),
        None => page,
    };
    Some(match path.as_slice() {
        ["explore" | "search_result", id] | ["discovery", "item", id] => {
            found.page(token(format!("{base}/explore/{id}")))
        }
        ["user", "profile", owner, id] => found
            .page(token(format!("{}/{id}", user(owner))))
            .profile(user(owner)),
        ["user", "profile", owner] => found.profile(user(owner)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn notes_users_and_images() {
        page(
            &XIAOHONGSHU,
            "https://www.xiaohongshu.com/discovery/item/65880524000000000700a643",
            "https://www.xiaohongshu.com/explore/65880524000000000700a643",
        );
        profile(
            &XIAOHONGSHU,
            "https://www.xiaohongshu.com/user/profile/6234917d0000000010008cf8",
            "https://www.xiaohongshu.com/user/profile/6234917d0000000010008cf8",
        );
        file(
            &XIAOHONGSHU,
            "https://sns-webpic-qc.xhscdn.com/202405050857/60985d4963cfb500a9b0838667eb3adc/1000g00828idf6nofk05g5ohki5uk137o8beqcv8!nd_dft_wgth_webp_3",
            Some("https://ci.xiaohongshu.com/1000g00828idf6nofk05g5ohki5uk137o8beqcv8"),
        );
    }
}
