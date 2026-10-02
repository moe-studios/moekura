//! ArtStreet (MediBang): pictures and books (`medibang.com/picture/<id>/`),
//! authors, and images on CloudFront.

use super::parts::Parts;
use super::{ARTSTREET, SourceUrl};

const CDNS: &[&str] = &[
    "dthezntil550i.cloudfront.net",
    "dqmk835cy5zzx.cloudfront.net",
];

/// A picture or book id ends with its author's.
fn author_of(id: &str) -> Option<String> {
    let author: u64 = id.get(17..)?.parse().ok()?;
    Some(format!("https://medibang.com/author/{author}/"))
}

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let cdn = CDNS.contains(&p.host.as_str());
    if p.domain != "medibang.com" && !cdn {
        return None;
    }
    let found = SourceUrl::of(&ARTSTREET);
    let path = p.path();
    if cdn {
        return Some(match path.as_slice() {
            [dir, "latest", id, .., file] => found
                .file(format!(
                    "https://dthezntil550i.cloudfront.net/{dir}/latest/{id}/{file}"
                ))
                .page(format!("https://medibang.com/picture/{id}/"))
                .profile(author_of(id)),
            [_, "current", id, _] => found
                .file(None)
                .page(format!("https://medibang.com/book/{id}/"))
                .profile(author_of(id)),
            _ => found.file(None),
        });
    }
    Some(match path.as_slice() {
        ["picture", id] => found
            .page(format!("https://medibang.com/picture/{id}/"))
            .profile(author_of(id)),
        ["book" | "viewer", id] => found
            .page(format!("https://medibang.com/book/{id}/"))
            .profile(author_of(id)),
        ["author", id, ..] => found.profile(format!("https://medibang.com/author/{id}/")),
        ["u", id, ..] => found.profile(format!("https://medibang.com/u/{id}/")),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{on, page, profile};
    use super::*;

    #[test]
    fn pictures_and_authors() {
        page(
            &ARTSTREET,
            "https://medibang.com/picture/4b2112261505098280008769655/",
            "https://medibang.com/picture/4b2112261505098280008769655/",
        );
        let picture = on(
            &ARTSTREET,
            "https://medibang.com/picture/4b2112261505098280008769655/",
        );
        assert_eq!(
            picture.profile_url.as_deref(),
            Some("https://medibang.com/author/8769655/")
        );
        profile(
            &ARTSTREET,
            "https://medibang.com/u/16672238/gallery/?cat=illust",
            "https://medibang.com/u/16672238/",
        );
    }
}
