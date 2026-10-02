//! Grafolio: projects (`grafolio.ogq.me/project/detail/<id>`), profiles,
//! and images.

use super::parts::Parts;
use super::{GRAFOLIO, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if p.domain != "ogq.me" {
        return None;
    }
    let found = SourceUrl::of(&GRAFOLIO);
    let base = p.basename().unwrap_or_default();
    Some(match (p.sub.as_str(), p.path().as_slice()) {
        ("files.grafolio", ["preview", "v1", "content", "real", user, "IMAGE", _]) => found.file(
            format!("https://files.grafolio.ogq.me/real/{user}/IMAGE/{base}"),
        ),
        ("files.grafolio", ["real", _, "IMAGE", _]) => found.file(p.without_query()),
        ("grafolio", ["project", "detail", id]) => {
            found.page(format!("https://grafolio.ogq.me/project/detail/{id}"))
        }
        ("grafolio", ["profile", name, ..]) => {
            found.profile(format!("https://grafolio.ogq.me/profile/{name}/projects"))
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
    fn projects_profiles_and_images() {
        page(
            &GRAFOLIO,
            "https://grafolio.ogq.me/project/detail/ccb07e90bdce4a868737abfca5136413",
            "https://grafolio.ogq.me/project/detail/ccb07e90bdce4a868737abfca5136413",
        );
        profile(
            &GRAFOLIO,
            "https://grafolio.ogq.me/profile/리니/like",
            "https://grafolio.ogq.me/profile/리니/projects",
        );
        file(
            &GRAFOLIO,
            "https://files.grafolio.ogq.me/preview/v1/content/real/566beece588b3/IMAGE/4718c558-2de0-442f-bbd8-54428c4fae7c.jpg?type=THUMBNAIL",
            Some(
                "https://files.grafolio.ogq.me/real/566beece588b3/IMAGE/4718c558-2de0-442f-bbd8-54428c4fae7c.jpg",
            ),
        );
    }
}
