//! OpenSea: items (`opensea.io/item/<chain>/<contract>/<token>`), accounts,
//! and images on seadn.io.

use super::parts::Parts;
use super::{OPENSEA, SourceUrl};

const RESERVED: &[&str] = &[
    "about",
    "account",
    "activity",
    "assets",
    "item",
    "blog",
    "careers",
    "category",
    "collection",
    "drops",
    "learn",
    "partners",
    "privacy",
    "studio",
    "tos",
    "rankings",
    "accounts",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "opensea.io" | "openseauserdata.com" | "seadn.io"
    ) {
        return None;
    }
    let found = SourceUrl::of(&OPENSEA);
    let path = p.path();
    if p.domain != "opensea.io" {
        return Some(match (p.sub.as_str(), path.as_slice()) {
            ("i", ["gae", file]) => {
                found.file(format!("https://lh3.googleusercontent.com/{file}=d"))
            }
            ("i2c" | "raw2", _) => found.file(p.without_query().replacen("://i2c.", "://raw2.", 1)),
            _ => found.file(p.without_query()),
        });
    }
    let account = |name: &str| format!("https://opensea.io/{name}");
    Some(match path.as_slice() {
        ["assets" | "item", chain, contract, token] => found.page(format!(
            "https://opensea.io/item/{chain}/{contract}/{token}"
        )),
        ["accounts", id] if id.starts_with("0x") => found.profile(account(id)),
        [name, ..] if !RESERVED.contains(name) => found.profile(account(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn items_and_accounts() {
        page(
            &OPENSEA,
            "https://opensea.io/assets/matic/0x2953399124f0cbb46d2cbacd8a89cf0599974963/7336718172757865837",
            "https://opensea.io/item/matic/0x2953399124f0cbb46d2cbacd8a89cf0599974963/7336718172757865837",
        );
        profile(
            &OPENSEA,
            "https://opensea.io/tororotororo/created",
            "https://opensea.io/tororotororo",
        );
    }
}
