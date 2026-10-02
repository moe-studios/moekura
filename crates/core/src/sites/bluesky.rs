//! Bluesky: posts (`bsky.app/profile/<handle or did>/post/<id>`),
//! profiles, and image blobs.

use super::parts::Parts;
use super::{BLUESKY, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "bsky.app" | "bsky.social" | "bsky.network"
    ) {
        return None;
    }
    let found = SourceUrl::of(&BLUESKY);
    let profile = |actor: &str| format!("https://bsky.app/profile/{actor}");
    let blob = |did: &str, cid: &str| {
        format!("https://bsky.social/xrpc/com.atproto.sync.getBlob?did={did}&cid={cid}")
    };
    Some(match p.path().as_slice() {
        ["profile", actor] => found.profile(profile(actor)),
        ["profile", actor, "post", post] => found
            .page(format!("{}/post/{post}", profile(actor)))
            .profile(profile(actor)),
        ["img", _, "plain", did, cid] => {
            let cid = cid.split('@').next().unwrap_or(cid);
            found.file(blob(did, cid)).profile(profile(did))
        }
        ["xrpc", "com.atproto.sync.getBlob"] => match (p.param("did"), p.param("cid")) {
            (Some(did), Some(cid)) => found.file(blob(&did, &cid)).profile(profile(&did)),
            _ => found,
        },
        [] if p.domain == "bsky.social" && !p.sub.is_empty() => {
            found.profile(profile(&format!("{}.bsky.social", p.sub)))
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn posts_profiles_and_blobs() {
        page(
            &BLUESKY,
            "https://bsky.app/profile/ixy.bsky.social/post/3kkvo4d4jd32g",
            "https://bsky.app/profile/ixy.bsky.social/post/3kkvo4d4jd32g",
        );
        profile(
            &BLUESKY,
            "https://bsky.app/profile/did:plc:3jogsxcisdcdzwjobhxbav2w",
            "https://bsky.app/profile/did:plc:3jogsxcisdcdzwjobhxbav2w",
        );
        profile(
            &BLUESKY,
            "https://ixy.bsky.social",
            "https://bsky.app/profile/ixy.bsky.social",
        );
        file(
            &BLUESKY,
            "https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:3jogsxcisdcdzwjobhxbav2w/bafkreiawa4vn5k37h2mlpwuhaqmeog3hsfe3z47iot7reqxjlff6juyge4@jpeg",
            Some(
                "https://bsky.social/xrpc/com.atproto.sync.getBlob?did=did:plc:3jogsxcisdcdzwjobhxbav2w&cid=bafkreiawa4vn5k37h2mlpwuhaqmeog3hsfe3z47iot7reqxjlff6juyge4",
            ),
        );
    }
}
