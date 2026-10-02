//! Misskey: notes (`<instance>/notes/<id>`), Play pages and users on
//! misskey.io, misskey.art and misskey.design, and their files.

use super::parts::Parts;
use super::{MISSKEY_ART, MISSKEY_DESIGN, MISSKEY_IO, Site, SourceUrl};

fn site_of(p: &Parts) -> Option<&'static Site> {
    match p.domain.as_str() {
        "misskey.io" | "misskeyusercontent.com" | "misskeyusercontent.jp" => Some(&MISSKEY_IO),
        "arkjp.net" if matches!(p.sub.as_str(), "s3" | "nos3") => Some(&MISSKEY_IO),
        "misskey.art" => Some(&MISSKEY_ART),
        "misskey.design" => Some(&MISSKEY_DESIGN),
        _ => None,
    }
}

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let site = site_of(p)?;
    let found = SourceUrl::of(site);
    // Files live on other hosts (media., files., file.).
    let is_instance = p.sub.is_empty() || p.sub == "www";
    if !is_instance {
        return Some(found.file(None));
    }
    let origin = format!("https://{}", p.domain);
    Some(match p.path().as_slice() {
        [name, ..] if name.starts_with('@') => found.profile(format!("{origin}/{name}")),
        ["users" | "user-info", id] => found.profile(format!("{origin}/users/{id}")),
        ["notes", id] => found.page(format!("{origin}/notes/{id}")),
        ["play", id] => found.page(format!("{origin}/play/{id}")),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn notes_users_and_files() {
        page(
            &MISSKEY_IO,
            "https://misskey.io/notes/9bxaf592x6",
            "https://misskey.io/notes/9bxaf592x6",
        );
        page(
            &MISSKEY_DESIGN,
            "https://misskey.design/notes/9r8c6x1n1p",
            "https://misskey.design/notes/9r8c6x1n1p",
        );
        profile(
            &MISSKEY_ART,
            "https://misskey.art/@ixy194/followers",
            "https://misskey.art/@ixy194",
        );
        profile(
            &MISSKEY_IO,
            "https://misskey.io/user-info/9bpemdns40",
            "https://misskey.io/users/9bpemdns40",
        );
        file(
            &MISSKEY_IO,
            "https://media.misskeyusercontent.jp/io/dfca7bd4-c073-4ea0-991f-313ab3a77847.png",
            None,
        );
    }
}
