//! Pixiv Factory: Palette collections on `factory.pixiv.net` and their
//! images (some hosted on Contentful).

use super::parts::Parts;
use super::{PIXIV_FACTORY, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let path = p.path();
    let contentful = p.domain == "ctfassets.net" && path.first() == Some(&"91hllu7j5j6t");
    if p.host != "factory.pixiv.net" && !contentful {
        return None;
    }
    let found = SourceUrl::of(&PIXIV_FACTORY);
    if contentful {
        return Some(found.file(p.without_query()));
    }
    let collection = |name: &str| format!("https://factory.pixiv.net/palette/collections/{name}");
    Some(match path.as_slice() {
        ["resources", "images", id, _] => found.file(format!(
            "https://factory.pixiv.net/resources/images/{id}/canvas"
        )),
        ["files", "uploads", ..] => found.file(None),
        ["_next", "image"] => found.file(
            p.param("url")
                .and_then(|u| super::parse(&u))
                .and_then(|u| u.file_url),
        ),
        ["palette", "collections", name] => {
            let image = p
                .fragment()
                .and_then(|f| f.strip_prefix("image-"))
                .filter(|id| super::parts::is_digits(id));
            match image {
                Some(id) => found.page(format!("{}#image-{id}", collection(name))),
                None => found.page(collection(name)),
            }
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page};
    use super::*;

    #[test]
    fn collections_and_images() {
        page(
            &PIXIV_FACTORY,
            "https://factory.pixiv.net/palette/collections/imys_tachie#image-13760863",
            "https://factory.pixiv.net/palette/collections/imys_tachie#image-13760863",
        );
        file(
            &PIXIV_FACTORY,
            "https://factory.pixiv.net/resources/images/13760863/thumb",
            Some("https://factory.pixiv.net/resources/images/13760863/canvas"),
        );
        file(
            &PIXIV_FACTORY,
            "https://images.ctfassets.net/91hllu7j5j6t/55IY8dLGAZnQuRIdQQLtE9/c4705fa83c046b5938beb6d2470550f8/Thumbnail_hasuimo2.jpg?w=384&q=75",
            Some(
                "https://images.ctfassets.net/91hllu7j5j6t/55IY8dLGAZnQuRIdQQLtE9/c4705fa83c046b5938beb6d2470550f8/Thumbnail_hasuimo2.jpg",
            ),
        );
    }
}
