//! Niconico: Seiga illustrations (`seiga.nicovideo.jp/seiga/im<id>`),
//! manga, videos, users, and images on nicoseiga.jp.

use super::parts::{Parts, is_digits, leading_digits};
use super::{NICO_SEIGA, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "nicovideo.jp" | "nicoseiga.jp" | "nicomanga.jp" | "nimg.jp" | "nico.ms"
    ) {
        return None;
    }
    let found = SourceUrl::of(&NICO_SEIGA);
    let illust = |id: &str| format!("https://seiga.nicovideo.jp/seiga/im{id}");
    let manga = |id: &str| format!("https://manga.nicovideo.jp/watch/mg{id}");
    let video = |id: &str| format!("https://www.nicovideo.jp/watch/{id}");
    let path = p.path();
    let sub = p.sub.as_str();
    let seiga = sub.ends_with("seiga");
    if matches!(
        p.domain.as_str(),
        "nicoseiga.jp" | "nimg.jp" | "nicomanga.jp"
    ) {
        return Some(match (sub, path.as_slice()) {
            ("lohas", ["o", .., id]) if is_digits(id) => found.file(None).page(illust(id)),
            ("lohas", ["thumb", id]) if id.ends_with(['i', 'q']) => {
                found.file(None).page(illust(&id[..id.len() - 1]))
            }
            _ => found.file(None),
        });
    }
    Some(match path.as_slice() {
        ["seiga", id] if seiga => match id.strip_prefix("im").and_then(leading_digits) {
            Some(id) => found.page(illust(id)),
            None => found,
        },
        ["watch", id] if seiga || sub.ends_with("manga") => {
            match id.strip_prefix("mg").and_then(leading_digits) {
                Some(id) => found.page(manga(id)),
                None => found,
            }
        }
        ["watch", id] => found.page(video(id)),
        ["image", ..] if seiga => found.file(None),
        [id] if p.domain == "nico.ms" => {
            if let Some(n) = id.strip_prefix("im").filter(|n| is_digits(n)) {
                found.page(illust(n))
            } else if let Some(n) = id.strip_prefix("mg").filter(|n| is_digits(n)) {
                found.page(manga(n))
            } else {
                found.page(video(id))
            }
        }
        ["user", "illust", id] if seiga => {
            found.profile(format!("https://seiga.nicovideo.jp/user/illust/{id}"))
        }
        ["manga", "list"] if seiga => match p.param("user_id") {
            Some(id) => found.profile(format!(
                "https://seiga.nicovideo.jp/manga/list?user_id={id}"
            )),
            None => found,
        },
        ["user", id, ..] if is_digits(id) => {
            let host = if sub == "commons" {
                "commons.nicovideo.jp"
            } else {
                "www.nicovideo.jp"
            };
            found.profile(format!("https://{host}/user/{id}"))
        }
        ["oekaki" | "oekaki_id", id] if sub == "dic" => match leading_digits(id) {
            Some(id) => found.page(format!("https://dic.nicovideo.jp/oekaki_id/{id}")),
            None => found,
        },
        ["u", id] if sub == "dic" => found.profile(format!("https://dic.nicovideo.jp/u/{id}")),
        ["users", id, ..] if matches!(sub, "q" | "3d") => {
            found.profile(format!("https://{sub}.nicovideo.jp/users/{id}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn illustrations_manga_and_users() {
        page(
            &NICO_SEIGA,
            "https://seiga.nicovideo.jp/seiga/im3521156",
            "https://seiga.nicovideo.jp/seiga/im3521156",
        );
        page(
            &NICO_SEIGA,
            "https://seiga.nicovideo.jp/watch/mg925907",
            "https://manga.nicovideo.jp/watch/mg925907",
        );
        page(
            &NICO_SEIGA,
            "https://nico.ms/im10922621",
            "https://seiga.nicovideo.jp/seiga/im10922621",
        );
        page(
            &NICO_SEIGA,
            "https://www.nicovideo.jp/watch/sm36465441",
            "https://www.nicovideo.jp/watch/sm36465441",
        );
        profile(
            &NICO_SEIGA,
            "https://sp.seiga.nicovideo.jp/user/illust/20542122",
            "https://seiga.nicovideo.jp/user/illust/20542122",
        );
        profile(
            &NICO_SEIGA,
            "https://www.nicovideo.jp/user/20446930/mylist/28674289",
            "https://www.nicovideo.jp/user/20446930",
        );
    }
}
