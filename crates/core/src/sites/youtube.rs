//! YouTube: videos, playlists and community posts, channels (handles,
//! user names, channel ids), and images.

use super::parts::Parts;
use super::{SourceUrl, YOUTUBE};

const RESERVED: &[&str] = &[
    "about",
    "account",
    "ads",
    "c",
    "channel",
    "creators",
    "feed",
    "gaming",
    "learn",
    "new",
    "playables",
    "playlist",
    "podcasts",
    "post",
    "premium",
    "results",
    "shorts",
    "t",
    "user",
    "vi",
    "watch",
    "embed",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let yt_images = p.sub.starts_with("yt")
        && p.sub[2..].chars().all(|c| c.is_ascii_digit())
        && matches!(p.domain.as_str(), "ggpht.com" | "googleusercontent.com");
    if !matches!(p.domain.as_str(), "youtube.com" | "youtu.be" | "ytimg.com") && !yt_images {
        return None;
    }
    let found = SourceUrl::of(&YOUTUBE);
    let base = "https://www.youtube.com";
    let video = |id: &str| format!("{base}/watch?v={id}");
    let path = p.path();
    if yt_images {
        let (dirs, id) = path.split_at(path.len().saturating_sub(1));
        let id = id
            .first()
            .map(|i| i.split('=').next().unwrap_or(i))
            .unwrap_or_default();
        let mut parts = vec![p.origin()];
        parts.extend(dirs.iter().map(|d| (*d).to_owned()));
        parts.push(format!("{id}=d"));
        return Some(found.file(parts.join("/")));
    }
    if p.domain == "ytimg.com" || p.sub == "img" {
        return Some(match path.as_slice() {
            ["vi", id, _] => found.file(None).page(video(id)),
            _ => found.file(None),
        });
    }
    if p.domain == "youtu.be" {
        return Some(match path.as_slice() {
            [id] => found.page(video(id)),
            _ => found,
        });
    }
    Some(match path.as_slice() {
        [handle, ..] if handle.starts_with('@') => found.profile(format!("{base}/{handle}")),
        ["user", name, ..] => found.profile(format!("{base}/user/{name}")),
        ["c", name, ..] => found.profile(format!("{base}/c/{name}")),
        ["channel", id, ..] => {
            let found = found.profile(format!("{base}/channel/{id}"));
            match p.param("lb") {
                Some(post) => found.page(format!("{base}/post/{post}")),
                None => found,
            }
        }
        ["watch"] => match p.param("v") {
            Some(id) => found.page(video(&id)),
            None => found,
        },
        ["shorts" | "embed", id] => found.page(video(id)),
        ["post", id] => found.page(format!("{base}/post/{id}")),
        ["playlist"] => match p.param("list") {
            Some(list) => found.page(format!("https://music.youtube.com/playlist?list={list}")),
            None => found,
        },
        [name] if !RESERVED.contains(name) => found.profile(format!("{base}/{name}")),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn videos_and_channels() {
        page(
            &YOUTUBE,
            "https://youtu.be/dQw4w9WgXcQ?si=i9hAbs3VV0ewqq6F",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        );
        page(
            &YOUTUBE,
            "https://www.youtube.com/shorts/GSR2ghvoTDY",
            "https://www.youtube.com/watch?v=GSR2ghvoTDY",
        );
        profile(
            &YOUTUBE,
            "https://www.youtube.com/@nonomaRui",
            "https://www.youtube.com/@nonomaRui",
        );
        profile(
            &YOUTUBE,
            "https://www.youtube.com/channel/UCfrCa2Y6VulwHD3eNd3HBRA",
            "https://www.youtube.com/channel/UCfrCa2Y6VulwHD3eNd3HBRA",
        );
        profile(
            &YOUTUBE,
            "https://www.youtube.com/SiplickIshida",
            "https://www.youtube.com/SiplickIshida",
        );
    }
}
