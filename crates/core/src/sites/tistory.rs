//! Tistory blogs (`<blog>.tistory.com`): posts by number or title, and old
//! image hosts. Their images on daumcdn.net are Kakao's.

use super::parts::{Parts, is_digits};
use super::{SourceUrl, TISTORY};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "tistory.com" {
        return None;
    }
    let found = SourceUrl::of(&TISTORY);
    let image_host = p.sub.starts_with("cfs") || p.sub.starts_with("cfile");
    if image_host {
        return Some(found.file(None));
    }
    if matches!(p.sub.as_str(), "" | "www") {
        return Some(found);
    }
    let blog = format!("https://{}.tistory.com", p.sub);
    let path = p.path();
    let path = path.strip_prefix(&["m"]).unwrap_or(&path);
    Some(match path {
        [id] if is_digits(id) => found.page(format!("{blog}/{id}")).profile(blog),
        ["entry", title] => found.page(format!("{blog}/entry/{title}")).profile(blog),
        _ => found.profile(blog),
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn posts_and_blogs() {
        page(
            &TISTORY,
            "https://primemeeting.tistory.com/m/25",
            "https://primemeeting.tistory.com/25",
        );
        page(
            &TISTORY,
            "https://caswac1.tistory.com/m/entry/용사의-선택지가-이상하다",
            "https://caswac1.tistory.com/entry/용사의-선택지가-이상하다",
        );
        profile(
            &TISTORY,
            "https://primemeeting.tistory.com/m",
            "https://primemeeting.tistory.com",
        );
    }
}
