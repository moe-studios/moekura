//! Hentai Foundry: pictures (`/pictures/user/<user>/<id>`), users, and
//! files on pictures.hentai-foundry.com.

use super::parts::{Parts, is_digits, leading_digits};
use super::{HENTAI_FOUNDRY, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "hentai-foundry.com" {
        return None;
    }
    let found = SourceUrl::of(&HENTAI_FOUNDRY);
    let page = |user: Option<&str>, id: &str| match user {
        Some(user) => format!("https://www.hentai-foundry.com/pictures/user/{user}/{id}"),
        None => format!("https://www.hentai-foundry.com/pic-{id}"),
    };
    let profile = |user: &str| format!("https://www.hentai-foundry.com/user/{user}");
    let work = |user: &str, id: &str, file: bool| {
        let found = found
            .clone()
            .page(page(Some(user), id))
            .profile(profile(user));
        if file {
            found.file(p.without_query())
        } else {
            found
        }
    };
    let path = p.path();
    Some(match (p.sub.as_str(), path.as_slice()) {
        ("pictures", [_, user, id, _]) if is_digits(id) => work(user, id, true),
        ("pictures", [_, user, file]) => match leading_digits(file) {
            Some(id) => work(user, id, true),
            None => found.file(None),
        },
        (_, ["piccies", _, user, file]) => match leading_digits(file) {
            Some(id) => work(user, id, true),
            None => found.file(None),
        },
        (_, ["pictures", "user", user, id, ..]) if is_digits(id) => work(user, id, false),
        ("thumbs", ["thumb.php"]) => match p.param("pid") {
            Some(id) => found.file(None).page(page(None, &id)),
            None => found.file(None),
        },
        (_, [pic]) if pic.starts_with("pic") && pic.contains('-') => {
            let id = pic.split('-').nth(1).and_then(leading_digits);
            match id {
                Some(id) => found.page(page(None, id)),
                None => found,
            }
        }
        (_, ["user" | "pictures", user, ..]) if *user != "user" => found.profile(profile(user)),
        (_, ["pictures", "user", user, ..]) => found.profile(profile(user)),
        (_, [php]) if php.ends_with(".php") => {
            let name = php
                .strip_prefix("user-")
                .or_else(|| php.strip_prefix("profile-"))
                .map(|n| n.trim_end_matches(".php"));
            match name {
                Some(name) => found.profile(profile(name)),
                None => found,
            }
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn pictures_and_users() {
        page(
            &HENTAI_FOUNDRY,
            "https://www.hentai-foundry.com/pictures/user/Afrobull/795025/kuroeda",
            "https://www.hentai-foundry.com/pictures/user/Afrobull/795025",
        );
        page(
            &HENTAI_FOUNDRY,
            "http://www.hentai-foundry.com/pic-149160.html",
            "https://www.hentai-foundry.com/pic-149160",
        );
        profile(
            &HENTAI_FOUNDRY,
            "https://www.hentai-foundry.com/user/kajinman/profile",
            "https://www.hentai-foundry.com/user/kajinman",
        );
        profile(
            &HENTAI_FOUNDRY,
            "https://www.hentai-foundry.com/pictures/user/kajinman/scraps",
            "https://www.hentai-foundry.com/user/kajinman",
        );
        profile(
            &HENTAI_FOUNDRY,
            "http://www.hentai-foundry.com/user-RockCandy.php",
            "https://www.hentai-foundry.com/user/RockCandy",
        );
    }
}
