//! Nijie: illustrations (`nijie.info/view.php?id=<id>`), members, and
//! images on pic.nijie.net (`__rs_…` samples).

use super::parts::{Parts, is_digits};
use super::{NIJIE, SourceUrl};

/// The work and member in an image's name, as far as it says.
fn from_file_name(stem: &str) -> (Option<&str>, Option<&str>) {
    let pieces: Vec<&str> = stem.split('_').collect();
    let stamp = |s: &str| s.len() == 14 && is_digits(s);
    match pieces.as_slice() {
        // <user>_<timestamp>[_…]
        [user, time, ..] if is_digits(user) && stamp(time) => (None, Some(user)),
        // <work>_<n>_<user>_<timestamp>
        [w, n, user, time] if is_digits(w) && is_digits(n) && is_digits(user) && stamp(time) => {
            ((*w != "0").then_some(*w), Some(user))
        }
        // <work>_<user>_<timestamp>_<n>
        [w, user, time, n] if is_digits(w) && is_digits(user) && stamp(time) && is_digits(n) => {
            (Some(w), Some(user))
        }
        // <work>_<n>_<hex>_<hex>
        [w, n, ..] if is_digits(w) && is_digits(n) && pieces.len() == 4 => {
            ((*w != "0").then_some(*w), None)
        }
        _ => (None, None),
    }
}

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "nijie.net" | "nijie.info") {
        return None;
    }
    let found = SourceUrl::of(&NIJIE);
    let page = |id: &str| format!("https://nijie.info/view.php?id={id}");
    let member = |id: &str| format!("https://nijie.info/members.php?id={id}");
    let path = p.path();
    if p.sub.starts_with("pic") {
        let full = p
            .as_str()
            .split('/')
            .filter(|s| !s.starts_with("__rs_"))
            .collect::<Vec<_>>()
            .join("/")
            .replacen("http:", "https:", 1);
        let (work, mut user) = p.stem().map(from_file_name).unwrap_or((None, None));
        // …/nijie/17/95/<user>/illust/<file>
        if let Some(at) = path.iter().position(|s| *s == "illust")
            && at >= 1
            && is_digits(path[at - 1])
        {
            user = Some(path[at - 1]);
        }
        return Some(
            found
                .file(full)
                .page(work.map(page))
                .profile(user.map(member)),
        );
    }
    Some(match path.as_slice() {
        ["view.php" | "view_popup.php"] => match p.param("id").filter(|id| is_digits(id)) {
            Some(id) => found.page(page(&id)),
            None => found,
        },
        ["members.php" | "members_illust.php" | "members_dojin.php"] => {
            match p.param("id").filter(|id| is_digits(id)) {
                Some(id) => found.profile(member(&id)),
                None => found,
            }
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{on, page, profile};
    use super::*;

    #[test]
    fn works_members_and_images() {
        page(
            &NIJIE,
            "https://nijie.info/view_popup.php?id=218856",
            "https://nijie.info/view.php?id=218856",
        );
        profile(
            &NIJIE,
            "https://nijie.info/members_illust.php?id=236014",
            "https://nijie.info/members.php?id=236014",
        );
        let image = on(
            &NIJIE,
            "https://pic01.nijie.info/nijie_picture/diff/main/218856_0_236014_20170620101329.png",
        );
        assert_eq!(
            image.page_url.as_deref(),
            Some("https://nijie.info/view.php?id=218856")
        );
        assert_eq!(
            image.profile_url.as_deref(),
            Some("https://nijie.info/members.php?id=236014")
        );
        let image = on(
            &NIJIE,
            "https://pic.nijie.net/06/__rs_l120x120/nijie/17/14/236014/illust/218856_1_7646cf57f6f1c695_f2ed81.png",
        );
        assert_eq!(
            image.file_url.as_deref(),
            Some(
                "https://pic.nijie.net/06/nijie/17/14/236014/illust/218856_1_7646cf57f6f1c695_f2ed81.png"
            )
        );
    }
}
