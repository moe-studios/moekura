//! Booth: items (`booth.pm/<lang>/items/<id>`, `<shop>.booth.pm/items/<id>`),
//! shops, and item images on `booth.pximg.net`.

use super::parts::{Parts, is_digits, is_uuid};
use super::{BOOTH, SourceUrl};

const RESERVED: &[&str] = &["", "www", "s", "s2", "asset", "accounts", "api", "manage"];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "booth.pm" && p.host != "booth.pximg.net" {
        return None;
    }
    let found = SourceUrl::of(&BOOTH);
    let item = |id: &str| format!("https://booth.pm/en/items/{id}");
    let path = p.path();
    if matches!(
        p.host.as_str(),
        "booth.pximg.net" | "s.booth.pm" | "s2.booth.pm"
    ) {
        // `_base_resized` files are samples; the original's extension
        // isn't known.
        let sample = p.basename().is_some_and(|b| b.contains("_base_resized"));
        let full = (!sample).then(|| p.without_query());
        let at = path.iter().position(|s| is_uuid(s));
        return Some(match at.map(|at| &path[at..]) {
            Some([_, "i", id, _]) if is_digits(id) => found.file(full).page(item(id)),
            _ => found.file(full),
        });
    }
    let shop = (!RESERVED.contains(&p.sub.as_str())).then_some(p.sub.as_str());
    let profile = |name: &str| format!("https://{name}.booth.pm");
    Some(match (shop, path.as_slice()) {
        (None, [_, "items", id]) | (None, ["items", id]) => found.page(item(id)),
        (Some(name), ["items", id]) => found.page(item(id)).profile(profile(name)),
        (Some(name), _) => found.profile(profile(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn items_shops_and_images() {
        page(
            &BOOTH,
            "https://booth.pm/ja/items/2864768",
            "https://booth.pm/en/items/2864768",
        );
        page(
            &BOOTH,
            "https://re-face.booth.pm/items/3435711",
            "https://booth.pm/en/items/3435711",
        );
        profile(
            &BOOTH,
            "https://re-face.booth.pm/items",
            "https://re-face.booth.pm",
        );
        file(
            &BOOTH,
            "https://booth.pximg.net/8bb9e4e3-d171-4027-88df-84480480f79d/i/2864768/00cdfef0-e8d5-454b-8554-4885a7e4827d.jpeg",
            Some(
                "https://booth.pximg.net/8bb9e4e3-d171-4027-88df-84480480f79d/i/2864768/00cdfef0-e8d5-454b-8554-4885a7e4827d.jpeg",
            ),
        );
    }
}
