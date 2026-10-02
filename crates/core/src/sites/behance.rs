//! Behance: projects (`behance.net/gallery/<id>/<title>`), users, and
//! project images (`…/project_modules/<size>/…`, the original `source`).

use super::parts::Parts;
use super::{BEHANCE, SourceUrl};

const RESERVED: &[&str] = &[
    "about",
    "assets",
    "auth",
    "blog",
    "careers",
    "entries",
    "galleries",
    "gallery",
    "hc",
    "hire",
    "misc",
    "joblist",
    "pro",
    "search",
    "services",
    "updates",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "behance.net" {
        return None;
    }
    let found = SourceUrl::of(&BEHANCE);
    let gallery = |id: &str, title: &str| format!("https://www.behance.net/gallery/{id}/{title}");
    // `ea4c7e97612065.5ec92bae8dc45.jpg`: 6 hex digits, then the project.
    let project_of = |file: &str| {
        let head = file.split('.').next()?;
        head.get(6..)
            .filter(|id| super::parts::is_digits(id))
            .map(str::to_owned)
    };
    Some(match p.path().as_slice() {
        ["project_modules", _, file] => found
            .file(format!("https://{}/project_modules/source/{file}", p.host))
            .page(project_of(file).map(|id| gallery(&id, "Title"))),
        ["v1", "rendition", "project_modules", _, file] => found
            .file(format!(
                "https://{}/v1/rendition/project_modules/source/{file}",
                p.host
            ))
            .page(project_of(file).map(|id| gallery(&id, "Title"))),
        ["gallery", id, title, ..] => found.page(gallery(id, title)),
        ["gallery", id] => found.page(gallery(id, "Title")),
        [name, ..] if !RESERVED.contains(name) && !p.sub.contains("cdn") => {
            found.profile(format!("https://www.behance.net/{name}"))
        }
        _ if p.has_file_ext() => found.file(None),
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page, profile};
    use super::*;

    #[test]
    fn projects_users_and_images() {
        page(
            &BEHANCE,
            "https://www.behance.net/gallery/97612065/SailorMoon/modules/563634913",
            "https://www.behance.net/gallery/97612065/SailorMoon",
        );
        profile(
            &BEHANCE,
            "https://www.behance.net/Kensukecreations/projects",
            "https://www.behance.net/Kensukecreations",
        );
        file(
            &BEHANCE,
            "https://mir-s3-cdn-cf.behance.net/project_modules/max_1200/ea4c7e97612065.5ec92bae8dc45.jpg",
            Some(
                "https://mir-s3-cdn-cf.behance.net/project_modules/source/ea4c7e97612065.5ec92bae8dc45.jpg",
            ),
        );
    }
}
