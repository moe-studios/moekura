//! DC Inside: gallery posts (`gall.dcinside.com/mgallery/board/view/?id=…&no=…`,
//! `m.dcinside.com/board/<id>/<no>`), user logs, and images.

use super::parts::Parts;
use super::{DC_INSIDE, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "dcinside.co.kr" | "dcinside.com") {
        return None;
    }
    let found = SourceUrl::of(&DC_INSIDE);
    let post = |board: &str, no: &str| {
        format!("https://gall.dcinside.com/mgallery/board/view/?id={board}&no={no}")
    };
    let user = |name: &str| format!("https://gallog.dcinside.com/{name}");
    Some(match (p.sub.as_str(), p.path().as_slice()) {
        (_, ["viewimage.php" | "viewimagePop.php"]) => match p.param("no") {
            Some(no) => found.file(format!("https://{}/viewimage.php?no={no}", p.host)),
            None => found.file(None),
        },
        ("gall", [.., "board", "view"]) => match (p.param("id"), p.param("no")) {
            (Some(board), Some(no)) => found.page(post(&board, &no)),
            _ => found,
        },
        ("m", ["board", board, no]) => found.page(post(board, no)),
        ("gallog", [name, ..]) | ("m", ["gallog", name, ..]) => found.profile(user(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn posts_and_users() {
        page(
            &DC_INSIDE,
            "https://m.dcinside.com/board/projectmx/11076518",
            "https://gall.dcinside.com/mgallery/board/view/?id=projectmx&no=11076518",
        );
        page(
            &DC_INSIDE,
            "https://gall.dcinside.com/mgallery/board/view/?id=projectmx&no=11076518&page=1",
            "https://gall.dcinside.com/mgallery/board/view/?id=projectmx&no=11076518",
        );
        profile(
            &DC_INSIDE,
            "https://m.dcinside.com/gallog/mannack0106",
            "https://gallog.dcinside.com/mannack0106",
        );
    }
}
