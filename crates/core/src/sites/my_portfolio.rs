//! Adobe Portfolio sites on `<name>.myportfolio.com` (sites on their own
//! domains can't be told apart from any other).

use super::parts::{Parts, is_uuid};
use super::{ADOBE_PORTFOLIO, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "myportfolio.com" {
        return None;
    }
    let found = SourceUrl::of(&ADOBE_PORTFOLIO);
    let path = p.path();
    let cdn = p.sub == "cdn" || p.sub.contains("-cdn-");
    if cdn {
        // `<uuid>_rw_1200.jpg` is a sample of `<uuid>.jpg`; the `?h=`
        // signature differs, so the strategy finds the original.
        let sample = p.stem().is_some_and(|s| s.contains('_'));
        return Some(found.file((!sample).then(|| p.as_str().to_owned())));
    }
    if matches!(p.sub.as_str(), "" | "www") {
        return Some(found);
    }
    let profile = format!("https://{}.myportfolio.com", p.sub);
    Some(match path.as_slice() {
        [page] if !is_uuid(page) && !p.has_file_ext() => {
            found.page(format!("{profile}/{page}")).profile(profile)
        }
        _ => found.profile(profile),
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn pages_and_files() {
        page(
            &ADOBE_PORTFOLIO,
            "https://sekigahara023.myportfolio.com/eaapexlegends5",
            "https://sekigahara023.myportfolio.com/eaapexlegends5",
        );
        profile(
            &ADOBE_PORTFOLIO,
            "https://sekigahara023.myportfolio.com/",
            "https://sekigahara023.myportfolio.com",
        );
        file(
            &ADOBE_PORTFOLIO,
            "https://cdn.myportfolio.com/86bfb012-1d8f-427f-bbbb-287c3b8c0057/bb0394ab-0ffd-414b-9748-2a8a751c645a_rw_1200.png?h=fdde829a19fbd8534d6f85d3914f419c",
            None,
        );
    }
}
