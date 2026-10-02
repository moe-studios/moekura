//! Kakao's image servers (daumcdn.net, kakaocdn.net), behind Tistory,
//! Daum and other Kakao sites.

use super::parts::Parts;
use super::{KAKAO, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "daumcdn.net" | "kakaocdn.net") {
        return None;
    }
    let found = SourceUrl::of(&KAKAO);
    Some(match p.path().as_slice() {
        // A thumbnail of the file in `fname`.
        ["thumb", ..] => match p.param("fname") {
            Some(inner) => {
                let full = super::parse(&inner)
                    .and_then(|u| u.file_url)
                    .unwrap_or(inner);
                found.file(full)
            }
            None => found.file(None),
        },
        ["cfile", "tistory", _] => found.file(format!("{}?original", p.without_query())),
        _ => found.file(p.without_query()),
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::file;
    use super::*;

    #[test]
    fn images() {
        file(
            &KAKAO,
            "https://img1.daumcdn.net/thumb/R1280x0/?scode=mtistory2&fname=https%3A%2F%2Fblog.kakaocdn.net%2Fdn%2FRA1tu%2FbtsFf2xGLbg%2FVzHK4tqMEWkeqUgDBxSkkK%2Fimg.jpg",
            Some("https://blog.kakaocdn.net/dn/RA1tu/btsFf2xGLbg/VzHK4tqMEWkeqUgDBxSkkK/img.jpg"),
        );
        file(
            &KAKAO,
            "https://t1.daumcdn.net/cfile/tistory/99A3CF4B5C2AFDF806",
            Some("https://t1.daumcdn.net/cfile/tistory/99A3CF4B5C2AFDF806?original"),
        );
    }
}
