//! Huashijie (pandapaint): works (`huashijie.art/work/detail/<id>`), market
//! products, users, and images.

use super::parts::{Parts, is_digits};
use super::{HUASHIJIE, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "huashijie.art" | "pandapaint.net") {
        return None;
    }
    let found = SourceUrl::of(&HUASHIJIE);
    let work = |id: &str| format!("https://www.huashijie.art/work/detail/{id}");
    let product = |id: &str| format!("https://www.huashijie.art/market/detail/{id}");
    let user = |id: &str| format!("https://www.huashijie.art/user/index/{id}");
    let path = p.path();
    if p.sub.starts_with("bsyimg") {
        return Some(match path.as_slice() {
            ["v2", _, "user", id, _] => found.file(p.without_query()).profile(user(id)),
            _ => found.file(p.without_query()),
        });
    }
    // The app's pages keep the id in the fragment: `#/detail?workId=1`.
    let fragment_id = |prefix: &str| {
        p.fragment()
            .and_then(|f| f.strip_prefix(prefix))
            .map(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .filter(|id| !id.is_empty())
    };
    Some(match (p.sub.as_str(), path.as_slice()) {
        ("" | "www", ["work", "detail", id]) | ("static", ["w_d", id]) => found.page(work(id)),
        ("" | "www", ["market", "detail", id]) | ("static", ["s_pd", id]) => {
            found.page(product(id))
        }
        ("" | "www", ["user", "index" | "shop", id]) if is_digits(id) => found.profile(user(id)),
        ("static", [_, "wap"]) => {
            if let Some(id) = fragment_id("/detail?workId=") {
                found.page(work(&id))
            } else if let Some(id) = fragment_id("/usercenter?userId=") {
                found.profile(user(&id))
            } else {
                found
            }
        }
        ("static", ["newmarket"]) => {
            if let Some(id) = fragment_id("/product/detail/") {
                found.page(product(&id))
            } else if let Some(id) = fragment_id("/usercenter/") {
                found.profile(user(&id))
            } else {
                found
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
    fn works_products_and_users() {
        page(
            &HUASHIJIE,
            "https://static.huashijie.art/w_d/235129335",
            "https://www.huashijie.art/work/detail/235129335",
        );
        page(
            &HUASHIJIE,
            "https://static.huashijie.art/hsj/wap/#/detail?workId=235129335",
            "https://www.huashijie.art/work/detail/235129335",
        );
        page(
            &HUASHIJIE,
            "https://static.huashijie.art/newmarket/?share=1&navbar=0#/product/detail/325923",
            "https://www.huashijie.art/market/detail/325923",
        );
        profile(
            &HUASHIJIE,
            "https://www.huashijie.art/user/shop/2381713",
            "https://www.huashijie.art/user/index/2381713",
        );
    }
}
