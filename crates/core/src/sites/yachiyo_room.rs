//! Yachiyo's Room: oekaki (`yachiyo-room.com/oekaki/<id>`), artists'
//! galleries, and images.

use super::parts::Parts;
use super::{SourceUrl, YACHIYO_ROOM};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let cdn = p.host == "d3icawwrjcmhat.cloudfront.net";
    if p.domain != "yachiyo-room.com" && !cdn {
        return None;
    }
    let found = SourceUrl::of(&YACHIYO_ROOM);
    if cdn {
        return Some(found.file(p.without_query()));
    }
    Some(match p.path().as_slice() {
        ["oekaki", id] => found.page(format!("https://yachiyo-room.com/oekaki/{id}")),
        ["gallery"] if p.param("name_mode").is_none_or(|m| m == "exact") => match p.param("name") {
            Some(name) => {
                let name = percent_encoding::utf8_percent_encode(
                    &name,
                    percent_encoding::NON_ALPHANUMERIC,
                );
                found.profile(format!("https://yachiyo-room.com/gallery?name={name}"))
            }
            None => found,
        },
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn oekaki_and_artists() {
        page(
            &YACHIYO_ROOM,
            "https://yachiyo-room.com/oekaki/1059",
            "https://yachiyo-room.com/oekaki/1059",
        );
        profile(
            &YACHIYO_ROOM,
            "https://yachiyo-room.com/gallery?name=abc&name_mode=exact",
            "https://yachiyo-room.com/gallery?name=abc",
        );
    }
}
