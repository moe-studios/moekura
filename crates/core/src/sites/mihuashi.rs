//! Mihuashi: artworks, stalls, projects, character cards and activity
//! works on mihuashi.com, profiles, and images (`…!sample` suffixes).

use super::parts::Parts;
use super::{MIHUASHI, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "mihuashi.com" {
        return None;
    }
    let found = SourceUrl::of(&MIHUASHI);
    let base = "https://www.mihuashi.com";
    if matches!(
        p.sub.as_str(),
        "image-assets" | "activity-assets" | "images"
    ) {
        // `…/file.jpg!sq300` is a sample of `…/file.jpg`.
        let full = p.without_query();
        let full = full.split('!').next().unwrap_or(&full).replace("pfop/", "");
        let full = full.replace("://images.mihuashi.com/", "://image-assets.mihuashi.com/");
        // permanent/<user>|-<year>/…
        let user = p
            .path()
            .iter()
            .find_map(|s| s.split_once("|-").map(|(user, _)| user.to_owned()))
            .filter(|u| super::parts::is_digits(u));
        return Some(
            found
                .file(full)
                .profile(user.map(|u| format!("{base}/profiles/{u}"))),
        );
    }
    Some(match p.path().as_slice() {
        [kind @ ("artworks" | "stalls" | "projects"), id] => {
            found.page(format!("{base}/{kind}/{id}"))
        }
        ["character-card", id, ..] => found.page(format!("{base}/character-card/{id}")),
        ["activities", activity, "artworks" | "activity_artworks", id] => {
            let page = format!("{base}/activities/{activity}/artworks/{id}");
            found.page(match p.param("type") {
                Some(kind) => format!("{page}?type={kind}"),
                None => page,
            })
        }
        ["profiles", id] => found.profile(format!("{base}/profiles/{id}")),
        ["users", name] => found.profile(format!("{base}/users/{name}")),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn works_profiles_and_images() {
        page(
            &MIHUASHI,
            "https://www.mihuashi.com/artworks/15092919",
            "https://www.mihuashi.com/artworks/15092919",
        );
        page(
            &MIHUASHI,
            "https://www.mihuashi.com/activities/jw3-exterior-12/artworks/10515?type=zjjh",
            "https://www.mihuashi.com/activities/jw3-exterior-12/artworks/10515?type=zjjh",
        );
        profile(
            &MIHUASHI,
            "https://www.mihuashi.com/profiles/29105?role=painter",
            "https://www.mihuashi.com/profiles/29105",
        );
        file(
            &MIHUASHI,
            "https://activity-assets.mihuashi.com/2021/07/04/01/FvJ4MjqshV3u2etTc_8-gD4vFfy-.jpg!artwork.detail",
            Some(
                "https://activity-assets.mihuashi.com/2021/07/04/01/FvJ4MjqshV3u2etTc_8-gD4vFfy-.jpg",
            ),
        );
    }
}
