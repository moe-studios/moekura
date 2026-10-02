//! Reddit: posts (`reddit.com/r/<sub>/comments/<id>/<title>`), comments,
//! users, and images on redd.it.

use super::parts::Parts;
use super::{REDDIT, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "reddit.com" | "redd.it" | "redditmedia.com"
    ) {
        return None;
    }
    let found = SourceUrl::of(&REDDIT);
    let base = "https://www.reddit.com";
    let user = |name: &str| format!("{base}/user/{name}");
    let path = p.path();
    if p.domain == "redd.it" && matches!(p.sub.as_str(), "i" | "preview") {
        // `<title>-<id>.jpg`: the id is what follows the last dash.
        let stem = p.stem().unwrap_or_default();
        let id = stem.rsplit('-').next().unwrap_or(stem);
        let ext = p.ext().unwrap_or_default();
        return Some(found.file(format!("https://i.redd.it/{id}.{ext}")));
    }
    if p.domain == "redd.it" && p.sub.is_empty() {
        return Some(match path.as_slice() {
            [id] => found.page(format!("{base}/comments/{id}")),
            _ => found,
        });
    }
    if p.domain == "redd.it" || p.has_file_ext() {
        return Some(found.file(None));
    }
    Some(match path.as_slice() {
        ["media"] => found.file(
            p.param("url")
                .and_then(|u| super::parse(&u))
                .and_then(|u| u.file_url),
        ),
        ["user" | "u", name, "comments", id, rest @ ..] => {
            let page = match rest.first() {
                Some(title) => format!("{}/comments/{id}/{title}", user(name)),
                None => format!("{base}/comments/{id}"),
            };
            found.page(page).profile(user(name))
        }
        ["user" | "u", name, ..] => found.profile(user(name)),
        ["r", sub, "comments", id, "comment", comment] => {
            found.page(format!("{base}/r/{sub}/comments/{id}/comment/{comment}"))
        }
        ["r", sub, "comments", id, title, ..] => {
            found.page(format!("{base}/r/{sub}/comments/{id}/{title}"))
        }
        ["r", sub, "comments", id] => found.page(format!("{base}/r/{sub}/comments/{id}")),
        ["r", sub, "s", share] => found.page(format!("{base}/r/{sub}/s/{share}")),
        ["comments", id, "comment", comment] => {
            found.page(format!("{base}/comments/{id}/comment/{comment}"))
        }
        ["comments" | "gallery", id] | ["mediaembed", id] => {
            found.page(format!("{base}/comments/{id}"))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_users_and_images() {
        page(
            &REDDIT,
            "https://old.reddit.com/r/arknights/comments/ttyccp/maria_nearl/",
            "https://www.reddit.com/r/arknights/comments/ttyccp/maria_nearl",
        );
        page(
            &REDDIT,
            "https://www.reddit.com/gallery/ttyccp",
            "https://www.reddit.com/comments/ttyccp",
        );
        page(
            &REDDIT,
            "https://redd.it/ttyccp",
            "https://www.reddit.com/comments/ttyccp",
        );
        profile(
            &REDDIT,
            "https://www.reddit.com/u/Valshier",
            "https://www.reddit.com/user/Valshier",
        );
        file(
            &REDDIT,
            "https://preview.redd.it/qoyhz3o8yde71.jpg?width=1440&format=pjpg",
            Some("https://i.redd.it/qoyhz3o8yde71.jpg"),
        );
    }
}
