//! Naver Blog (`blog.naver.com/<user>/<post>`) and Naver Cafe
//! (`cafe.naver.com/<club>/<article>`), and their images on pstatic.net.

use super::parts::{Parts, is_digits};
use super::{NAVER_BLOG, NAVER_CAFE, SourceUrl};

const RESERVED: &[&str] = &[
    "guestbook",
    "memo",
    "mylog",
    "prologue",
    "PostView.naver",
    "BlogUserInfo.naver",
];

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    if !matches!(
        p.domain.as_str(),
        "naver.com" | "naver.net" | "pstatic.net" | "blog.me"
    ) {
        return None;
    }
    if p.sub.contains("cafe") {
        return Some(cafe(p));
    }
    // Only the blog: Naver's other sites (Post, Comic) aren't recognised.
    if p.domain == "naver.com" && !matches!(p.sub.as_str(), "blog" | "m.blog" | "rss.blog") {
        return None;
    }
    let found = SourceUrl::of(&NAVER_BLOG);
    let blog = |name: &str| format!("https://blog.naver.com/{name}");
    if matches!(p.domain.as_str(), "naver.net" | "pstatic.net") {
        let full = if p.sub.ends_with("phinf") {
            p.without_query()
        } else if p.sub == "blogfiles" || p.sub.contains("postfiles") || p.sub.contains("blogthumb")
        {
            format!("http://blogfiles.naver.net{}", p.url.path())
        } else {
            return Some(found.file(None));
        };
        return Some(found.file(full));
    }
    if p.domain == "blog.me" {
        return Some(found.profile(blog(&p.sub)));
    }
    let path = p.path();
    Some(match (p.sub.as_str(), path.as_slice()) {
        ("rss.blog", [file]) => found.profile(blog(file.trim_end_matches(".xml"))),
        (_, [name, post]) if is_digits(post) => found
            .page(format!("{}/{post}", blog(name)))
            .profile(blog(name)),
        (_, _) if p.param("blogId").is_some() => {
            let name = p.param("blogId").unwrap_or_default();
            match p.param("logNo").filter(|n| is_digits(n)) {
                Some(post) => found
                    .page(format!("{}/{post}", blog(&name)))
                    .profile(blog(&name)),
                None => found.profile(blog(&name)),
            }
        }
        (_, [name, ..])
            if !RESERVED.contains(name) && !name.ends_with(".nhn") && !name.ends_with(".naver") =>
        {
            found.profile(blog(name))
        }
        _ => found,
    })
}

fn cafe(p: &Parts) -> SourceUrl {
    let found = SourceUrl::of(&NAVER_CAFE);
    let base = "https://cafe.naver.com";
    let article = |club: &str, id: &str| format!("{base}/ca-fe/cafes/{club}/articles/{id}");
    if matches!(p.domain.as_str(), "naver.net" | "pstatic.net") {
        return found.file(format!("http://cafefiles.naver.net{}", p.url.path()));
    }
    match p.path().as_slice() {
        ["ImageView.nhn"] | ["common", "storyphoto", "viewer.html"] => found.file(
            p.param("imageUrl")
                .or_else(|| p.param("src"))
                .and_then(|u| super::parse(&u))
                .and_then(|u| u.file_url),
        ),
        ["ca-fe" | "f-e", "cafes", club, "articles", id]
        | ["ca-fe", "web", "cafes", club, "articles", id] => found.page(article(club, id)),
        ["ca-fe", "cafes", club, "members", member] => {
            found.profile(format!("{base}/ca-fe/cafes/{club}/members/{member}"))
        }
        _ if p.param("clubid").is_some() => {
            let club = p.param("clubid").unwrap_or_default();
            match p.param("articleid") {
                Some(id) => found.page(article(&club, &id)),
                None => found.profile(format!("{base}/MyCafeIntro.nhn?clubid={club}")),
            }
        }
        [club, id] if is_digits(id) => found
            .page(format!("{base}/{club}/{id}"))
            .profile(format!("{base}/{club}")),
        [club] => found.profile(format!("{base}/{club}")),
        _ => found,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{page, profile};
    use super::*;

    #[test]
    fn blogs_and_cafes() {
        page(
            &NAVER_BLOG,
            "https://m.blog.naver.com/goam2/221647025085",
            "https://blog.naver.com/goam2/221647025085",
        );
        page(
            &NAVER_BLOG,
            "https://m.blog.naver.com/PostView.naver?blogId=fishtailia&logNo=223434964582",
            "https://blog.naver.com/fishtailia/223434964582",
        );
        profile(
            &NAVER_BLOG,
            "https://m.blog.naver.com/goam2?tab=1",
            "https://blog.naver.com/goam2",
        );
        page(
            &NAVER_CAFE,
            "https://m.cafe.naver.com/ca-fe/web/cafes/29767250/articles/785",
            "https://cafe.naver.com/ca-fe/cafes/29767250/articles/785",
        );
        page(
            &NAVER_CAFE,
            "https://cafe.naver.com/ArticleRead.nhn?clubid=29767250&articleid=793",
            "https://cafe.naver.com/ca-fe/cafes/29767250/articles/793",
        );
        profile(
            &NAVER_CAFE,
            "https://m.cafe.naver.com/masterofeternity",
            "https://cafe.naver.com/masterofeternity",
        );
    }
}
