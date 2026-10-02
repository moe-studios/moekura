//! Inkbunny: submissions (`inkbunny.net/s/<id>`), users, and files on
//! metapix.net.

use super::parts::{Parts, is_digits, leading_digits};
use super::{INKBUNNY, SourceUrl};

const RESERVED: &[&str] = &["s", "submissionview.php", "user.php", "j", "search.php"];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let files = p.host.ends_with(".ib.metapix.net");
    if p.domain != "inkbunny.net" && !files {
        return None;
    }
    let found = SourceUrl::of(&INKBUNNY);
    if files {
        return Some(found.file(None));
    }
    let page = |id: &str| format!("https://inkbunny.net/s/{id}");
    Some(match p.path().as_slice() {
        ["s", id] => match leading_digits(id) {
            Some(id) => found.page(page(id)),
            None => found,
        },
        ["submissionview.php"] => match p.param("id").filter(|id| is_digits(id)) {
            Some(id) => found.page(page(&id)),
            None => found,
        },
        ["user.php"] => match p.param("user_id").filter(|id| is_digits(id)) {
            Some(id) => found.profile(format!("https://inkbunny.net/user.php?user_id={id}")),
            None => found,
        },
        [name]
            if !RESERVED.contains(name)
                && !name.contains('.')
                && name.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            found.profile(format!("https://inkbunny.net/{name}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn submissions_and_users() {
        page(
            &INKBUNNY,
            "https://inkbunny.net/s/3200751-p2-",
            "https://inkbunny.net/s/3200751",
        );
        page(
            &INKBUNNY,
            "https://inkbunny.net/submissionview.php?id=3200751",
            "https://inkbunny.net/s/3200751",
        );
        profile(
            &INKBUNNY,
            "https://inkbunny.net/DAGASI",
            "https://inkbunny.net/DAGASI",
        );
    }
}
