//! DeviantArt: works (`deviantart.com/<user>/art/<title>-<id>`, the old
//! `<user>.deviantart.com/art/…`, `fav.me/d<base 36 id>`), Sta.sh, users,
//! and files on `deviantart.net` and wixmp.com, whose names often carry
//! the work's id.

use super::parts::{Parts, is_digits};
use super::{DEVIANTART, SourceUrl};

const RESERVED: &[&str] = &[
    "art",
    "deviation",
    "download",
    "users",
    "stash",
    "view",
    "view.php",
    "view-full.php",
];

/// A sample URL without its quality setting (`,q_80,strp`).
fn without_quality(url: &str) -> Option<String> {
    let at = url.find(",q_")?;
    let end = at + url[at..].find("/")?;
    Some(format!("{}{}", &url[..at], &url[end..]))
}

/// A base 36 number (`dbc3a48`'s `bc3a48`).
fn base36(s: &str) -> Option<u64> {
    u64::from_str_radix(s, 36).ok()
}

/// The work id and artist in a file's name: `<title>_by_<artist>-d<id>`,
/// `<hash>-d<id>`, or `d<id>-<uuid>`.
fn from_file_name(name: &str) -> (Option<u64>, Option<String>) {
    let name = name.split('.').next().unwrap_or(name);
    // d797tit-1eac22e0-38b6-4eae-adcb-1b72843fd62a
    if let Some(rest) = name.strip_prefix('d')
        && rest.len() > 7
        && rest.as_bytes()[6] == b'-'
        && super::parts::is_uuid(&rest[7..rest.len().min(43)])
    {
        return (base36(&rest[..6]), None);
    }
    if let Some((before, id)) = name.rsplit_once("-d").or_else(|| name.rsplit_once("_d")) {
        let id = id.split('-').next().unwrap_or(id);
        let artist = before
            .rsplit_once("_by_")
            .map(|(_, artist)| artist.replace('_', "-"));
        if id.chars().all(|c| c.is_ascii_alphanumeric()) && !id.is_empty() {
            return (base36(id), artist);
        }
    }
    let artist = name
        .rsplit_once("_by_")
        .map(|(_, artist)| artist.replace('_', "-"));
    (None, artist)
}

/// A work's page, `<title>-<id>` by `name`.
fn work(found: SourceUrl, name: &str, slug: &str) -> Option<SourceUrl> {
    let user = format!("https://www.deviantart.com/{name}");
    let (title, id) = slug.rsplit_once('-').unwrap_or(("", slug));
    if !is_digits(id) {
        return Some(found.profile(user));
    }
    let page = if title.is_empty() {
        format!("https://www.deviantart.com/deviation/{id}")
    } else {
        format!("https://www.deviantart.com/{name}/art/{title}-{id}")
    };
    Some(found.page(page).profile(user))
}

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let wixmp = p.domain == "wixmp.com"
        && matches!(
            p.sub.as_str(),
            "images-wixmp-ed30a86b8c4ca887773594c2"
                | "wixmp-ed30a86b8c4ca887773594c2"
                | "api-da"
                | "img-deviantart"
        );
    if !wixmp
        && !matches!(
            p.domain.as_str(),
            "deviantart.com"
                | "deviantart.net"
                | "fav.me"
                | "sta.sh"
                | "daportfolio.com"
                | "artworkfolio.com"
        )
    {
        return None;
    }
    let found = SourceUrl::of(&DEVIANTART);
    let deviation = |id: u64| format!("https://www.deviantart.com/deviation/{id}");
    let user = |name: &str| format!("https://www.deviantart.com/{name}");
    let path = p.path();
    if wixmp || p.domain == "deviantart.net" {
        // The file named in the path, before any `/v1/fill/…` sample part.
        let name = path
            .iter()
            .position(|s| *s == "v1")
            .map_or_else(|| p.basename(), |at| at.checked_sub(1).map(|at| path[at]))
            .unwrap_or_default();
        let (id, artist) = from_file_name(name);
        let found = match (id, &artist) {
            (Some(id), _) => found.page(deviation(id)),
            _ => found,
        };
        let found = match artist {
            Some(artist) => found.profile(user(&artist)),
            None => found,
        };
        // Samples of works up to 790677560 have their originals under
        // /intermediary/; newer ones are at their best at full quality.
        let full = (wixmp && path.first() == Some(&"f") && path.contains(&"v1")).then(|| {
            let at = path.iter().position(|s| *s == "v1").unwrap_or(path.len());
            let gif = p.ext().as_deref() == Some("gif")
                || path[..at].last().is_some_and(|f| f.ends_with(".gif"));
            if id.is_some_and(|id| id <= 790_677_560) && !gif {
                format!("https://{}/intermediary/{}", p.host, path[..at].join("/"))
            } else {
                let url = p.as_str().to_owned();
                match without_quality(&url) {
                    Some(cleaned) => cleaned,
                    None => url,
                }
            }
        });
        return Some(found.file(full));
    }
    if p.domain == "fav.me" {
        return Some(match path.as_slice() {
            [id] => match id.strip_prefix('d').and_then(base36) {
                Some(id) => found.page(deviation(id)),
                None => found,
            },
            _ => found,
        });
    }
    if p.domain == "sta.sh" {
        return Some(match path.as_slice() {
            [id] | ["zip", id] => found.page(format!("https://sta.sh/{id}")),
            _ => found,
        });
    }
    let sub = (!matches!(p.sub.as_str(), "" | "www")).then_some(p.sub.as_str());
    Some(match (sub, path.as_slice()) {
        (_, ["download", id, file]) if is_digits(id) => {
            let (_, artist) = from_file_name(file);
            let found = found.page(deviation(id.parse().ok()?)).file(None);
            match artist {
                Some(artist) => found.profile(user(&artist)),
                None => found,
            }
        }
        (_, ["deviation" | "view", id]) if is_digits(id) => found.page(deviation(id.parse().ok()?)),
        (_, ["view.php" | "view-full.php"]) => match p.param("id") {
            Some(id) if is_digits(&id) => found.page(deviation(id.parse().ok()?)),
            _ => found,
        },
        (_, ["stash", id]) => found.page(format!("https://sta.sh/{id}")),
        (None, [name, "art", slug, ..]) => work(found, name, slug)?,
        (Some(name), ["art", slug, ..]) => work(found, name, slug)?,
        (None, [name, ..]) if p.domain == "deviantart.com" && !RESERVED.contains(name) => {
            found.profile(user(name))
        }
        (Some(name), _) => found.profile(user(name)),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, on, page, profile};
    use super::*;

    #[test]
    fn works_users_and_files() {
        page(
            &DEVIANTART,
            "https://www.deviantart.com/noizave/art/test-post-please-ignore-685436408",
            "https://www.deviantart.com/noizave/art/test-post-please-ignore-685436408",
        );
        page(
            &DEVIANTART,
            "https://noizave.deviantart.com/art/test-post-please-ignore-685436408",
            "https://www.deviantart.com/noizave/art/test-post-please-ignore-685436408",
        );
        page(
            &DEVIANTART,
            "https://fav.me/dbc3a48",
            "https://www.deviantart.com/deviation/685436408",
        );
        page(
            &DEVIANTART,
            "https://www.deviantart.com/view.php?id=14864502",
            "https://www.deviantart.com/deviation/14864502",
        );
        profile(
            &DEVIANTART,
            "https://www.deviantart.com/noizave/gallery",
            "https://www.deviantart.com/noizave",
        );
        profile(
            &DEVIANTART,
            "https://noizave.deviantart.com",
            "https://www.deviantart.com/noizave",
        );
        let found = on(
            &DEVIANTART,
            "http://orig12.deviantart.net/9b69/f/2017/023/7/c/illustration___tokyo_encount_oei__by_melisaongmiqin-dawi58s.png",
        );
        assert!(found.is_file);
        assert_eq!(
            found.page_url.as_deref(),
            Some("https://www.deviantart.com/deviation/659256076")
        );
        assert_eq!(
            found.profile_url.as_deref(),
            Some("https://www.deviantart.com/melisaongmiqin")
        );
        file(
            &DEVIANTART,
            "https://images-wixmp-ed30a86b8c4ca887773594c2.wixmp.com/f/d8995973-0b32-4a7d-8cd8-d847d083689a/d797tit-1eac22e0-38b6-4eae-adcb-1b72843fd62a.png/v1/fill/w_720,h_1110,q_75,strp/goruto_by_xyelkiltrox-d797tit.png",
            Some(
                "https://images-wixmp-ed30a86b8c4ca887773594c2.wixmp.com/intermediary/f/d8995973-0b32-4a7d-8cd8-d847d083689a/d797tit-1eac22e0-38b6-4eae-adcb-1b72843fd62a.png",
            ),
        );
    }
}
