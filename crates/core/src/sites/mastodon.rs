//! Mastodon instances artists post on: Pawoo and Baraag. Statuses
//! (`@<user>/<id>`, `web/statuses/<id>`), accounts and media.

use super::parts::{Parts, is_digits};
use super::{BARAAG, PAWOO, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let site = match p.domain.as_str() {
        "pawoo.net" => &PAWOO,
        "baraag.net" => &BARAAG,
        _ => return None,
    };
    let found = SourceUrl::of(site);
    let origin = format!("https://{}", p.domain);
    let user = |name: &str| format!("{origin}/@{}", name.trim_start_matches('@'));
    let path = p.path();
    if let Some(at) = path.iter().position(|s| *s == "media_attachments") {
        // …/files/<dirs>/<size>/<file>: the original is `original`.
        let rest = &path[at + 1..];
        return Some(match rest {
            ["files", dirs @ .., _size, file] => {
                let host = if site.key == "baraag" {
                    "https://media.baraag.net".to_owned()
                } else {
                    "https://img.pawoo.net".to_owned()
                };
                found.file(format!(
                    "{host}/media_attachments/files/{}/original/{file}",
                    dirs.join("/")
                ))
            }
            _ => found.file(None),
        });
    }
    Some(match path.as_slice() {
        [name, id, ..] if name.starts_with('@') && is_digits(id) => found
            .page(format!("{}/{id}", user(name)))
            .profile(user(name)),
        [name, ..] | ["web", name, ..] if name.starts_with('@') => found.profile(user(name)),
        ["users", name, ..] => found.profile(user(name)),
        ["web", "statuses", id, ..] => found.page(format!("{origin}/web/statuses/{id}")),
        ["web", "accounts", id] => found.profile(format!("{origin}/web/accounts/{id}")),
        ["media", _] => found.file(None),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn statuses_accounts_and_media() {
        page(
            &PAWOO,
            "https://pawoo.net/@evazion/19451018",
            "https://pawoo.net/@evazion/19451018",
        );
        page(
            &BARAAG,
            "https://baraag.net/web/statuses/102270656480174153",
            "https://baraag.net/web/statuses/102270656480174153",
        );
        profile(
            &BARAAG,
            "https://baraag.net/@quietvice/media",
            "https://baraag.net/@quietvice",
        );
        profile(
            &PAWOO,
            "https://pawoo.net/users/esoraneko",
            "https://pawoo.net/@esoraneko",
        );
        profile(
            &BARAAG,
            "https://baraag.net/web/@loodncrood",
            "https://baraag.net/@loodncrood",
        );
        file(
            &BARAAG,
            "https://baraag.net/system/media_attachments/files/107/866/084/749/942/932/small/a9e0f553e332f303.mp4",
            Some(
                "https://media.baraag.net/media_attachments/files/107/866/084/749/942/932/original/a9e0f553e332f303.mp4",
            ),
        );
    }
}
