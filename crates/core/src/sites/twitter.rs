//! Twitter / X, and the proxies people share its links through
//! (fxtwitter, vxtwitter, fixupx, nitter): statuses, users and files on
//! `pbs.twimg.com`.

use super::parts::{Parts, is_digits};
use super::{SourceUrl, TWITTER};

const DOMAINS: &[&str] = &[
    "twitter.com",
    "twimg.com",
    "x.com",
    "t.co",
    "fxtwitter.com",
    "vxtwitter.com",
    "twittpr.com",
    "fixvx.com",
    "fixupx.com",
    "nitter.net",
    "xcancel.com",
];

/// Paths that aren't user names.
const RESERVED: &[&str] = &[
    "home",
    "explore",
    "i",
    "intent",
    "messages",
    "notifications",
    "privacy",
    "search",
    "tos",
    "settings",
    "hashtag",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !DOMAINS.contains(&p.domain.as_str()) {
        return None;
    }
    let found = SourceUrl::of(&TWITTER);
    let path = p.path();
    let status = |user: Option<&str>, id: &str| match user {
        Some(user) => format!("https://x.com/{user}/status/{id}"),
        None => format!("https://x.com/i/web/status/{id}"),
    };
    let user = |name: &str| format!("https://x.com/{}", name.trim_start_matches('@'));
    if p.domain == "twimg.com" {
        return Some(match (p.sub.as_str(), path.as_slice()) {
            (
                "pbs",
                [
                    kind @ ("media"
                    | "tweet_video_thumb"
                    | "ext_tw_video_thumb"
                    | "amplify_video_thumb"),
                    dirs @ ..,
                    file,
                ],
            ) => {
                // `ABC.jpg:small`, or `ABC?format=jpg&name=900x900`.
                let (file, size) = file.split_once(':').unwrap_or((file, ""));
                let (name, ext) = file.split_once('.').unwrap_or((file, ""));
                let ext = p.param("format").unwrap_or_else(|| ext.to_owned());
                // Without a size, Twitter serves a medium one.
                let size = p.param("name").unwrap_or_else(|| size.to_owned());
                let mut path = vec![*kind];
                path.extend_from_slice(dirs);
                found
                    .file((!ext.is_empty()).then(|| {
                        format!("https://pbs.twimg.com/{}/{name}.{ext}:orig", path.join("/"))
                    }))
                    .sample(size != "orig")
            }
            ("pbs", ["profile_banners", id, file, ..]) if is_digits(id) => found.file(format!(
                "https://pbs.twimg.com/profile_banners/{id}/{file}/1500x500"
            )),
            ("pbs", ["profile_images", id, file]) => {
                // `<name>_400x400.png`: the original drops the size.
                let ext = p.ext().unwrap_or_default();
                let name = file.get(..8).unwrap_or(file);
                found.file(format!(
                    "https://pbs.twimg.com/profile_images/{id}/{name}.{ext}"
                ))
            }
            _ => found.file(None),
        });
    }
    if p.domain == "t.co" || p.sub == "pic" {
        return Some(found);
    }
    Some(match path.as_slice() {
        ["i", "web", "status", id] if is_digits(id) => found.page(status(None, id)),
        [name, "status" | "statuses", id, ..] => {
            let id = id.split('.').next().unwrap_or(id);
            if !is_digits(id) {
                found
            } else if RESERVED.contains(name) {
                found.page(status(None, id))
            } else {
                let name = name.trim_start_matches('@');
                found.page(status(Some(name), id)).profile(user(name))
            }
        }
        ["intent", "user"] => match (p.param("user_id"), p.param("screen_name")) {
            (Some(id), _) if is_digits(&id) => found.profile(format!("https://x.com/i/user/{id}")),
            (_, Some(name)) => found.profile(user(&name)),
            _ => found,
        },
        ["intent", "favorite" | "retweet"] => match p.param("tweet_id") {
            Some(id) if is_digits(&id) => found.page(status(None, &id)),
            _ => found,
        },
        ["i", "user", id] if is_digits(id) => found.profile(format!("https://x.com/i/user/{id}")),
        [name, ..] if !RESERVED.contains(name) => found.profile(user(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile, sample};
    use super::*;

    #[test]
    fn statuses_users_and_files() {
        page(
            &TWITTER,
            "https://twitter.com/motty08111213/status/943446161586733056/photo/2",
            "https://x.com/motty08111213/status/943446161586733056",
        );
        page(
            &TWITTER,
            "https://fxtwitter.com/i/status/943446161586733056",
            "https://x.com/i/web/status/943446161586733056",
        );
        page(
            &TWITTER,
            "https://x.com/i/web/status/943446161586733056",
            "https://x.com/i/web/status/943446161586733056",
        );
        profile(
            &TWITTER,
            "https://mobile.twitter.com/motty08111213/likes",
            "https://x.com/motty08111213",
        );
        profile(
            &TWITTER,
            "https://x.com/intent/user?user_id=1485229827984531457",
            "https://x.com/i/user/1485229827984531457",
        );
        file(
            &TWITTER,
            "https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb.jpg:small",
            Some("https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb.jpg:orig"),
        );
        file(
            &TWITTER,
            "https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb?format=jpg&name=900x900",
            Some("https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb.jpg:orig"),
        );
        file(
            &TWITTER,
            "https://pbs.twimg.com/profile_images/1425792004877733891/UM8s9d2x_400x400.png",
            Some("https://pbs.twimg.com/profile_images/1425792004877733891/UM8s9d2x.png"),
        );
        for (raw, is_sample) in [
            ("https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb.jpg", true),
            (
                "https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb.jpg:small",
                true,
            ),
            (
                "https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb.jpg:orig",
                false,
            ),
            (
                "https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb?format=jpg&name=orig",
                false,
            ),
        ] {
            sample(&TWITTER, raw, is_sample);
        }
    }
}
