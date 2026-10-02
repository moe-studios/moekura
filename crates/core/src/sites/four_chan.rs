//! 4chan: threads (`boards.4chan.org/<board>/thread/<id>`) and files on
//! `i.4cdn.org`.

use super::parts::{Parts, leading_digits};
use super::{FOUR_CHAN, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(p.domain.as_str(), "4chan.org" | "4channel.org" | "4cdn.org") {
        return None;
    }
    let found = SourceUrl::of(&FOUR_CHAN);
    Some(match p.path().as_slice() {
        [board, "thread" | "res", id, ..] if p.domain != "4cdn.org" => {
            let Some(id) = leading_digits(id) else {
                return Some(found);
            };
            let post = p.fragment().filter(|f| f.starts_with('p'));
            let page = format!("https://boards.4chan.org/{board}/thread/{id}");
            found.page(match post {
                Some(post) => format!("{page}#{post}"),
                None => page,
            })
        }
        // `<id>s.jpg` is a thumbnail of `<id>.<ext>`, whose extension
        // isn't known.
        [_, file] | [_, "src", file] if p.has_file_ext() => {
            let thumb = p.stem().is_some_and(|s| s.ends_with('s'));
            let _ = file;
            found
                .file((!thumb).then(|| p.without_query()))
                .sample(thumb)
        }
        _ => found,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{file, page};
    use super::*;

    #[test]
    fn threads_and_files() {
        page(
            &FOUR_CHAN,
            "https://boards.4channel.org/vt/thread/37293562#p37294005",
            "https://boards.4chan.org/vt/thread/37293562#p37294005",
        );
        page(
            &FOUR_CHAN,
            "http://zip.4chan.org/jp/res/3598845.html",
            "https://boards.4chan.org/jp/thread/3598845",
        );
        file(
            &FOUR_CHAN,
            "https://i.4cdn.org/vt/1668729957824814.webm",
            Some("https://i.4cdn.org/vt/1668729957824814.webm"),
        );
        file(
            &FOUR_CHAN,
            "https://i.4cdn.org/vt/1668729957824814s.jpg",
            None,
        );
    }
}
