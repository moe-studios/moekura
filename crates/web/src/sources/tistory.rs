//! Tistory posts, from their mobile pages: the post's images (on Kakao's
//! servers, at their originals), the blog, tags and text.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text, tags_named};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    // https://<blog>.tistory.com/<n> -> …/m/<n>
    let (blog, post) = page.split_at(
        page.find(".tistory.com/")
            .map(|at| at + ".tistory.com/".len())
            .ok_or("Tistory: not a post")?,
    );
    let body = http.page(&format!("{blog}m/{post}"), &[]).await?;
    post_info(known, page, &body).ok_or_else(|| "Tistory: no post in the page".into())
}

fn post_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let (_, content) = html::find(body, "div", |t| t.has_class("blogview_content"))?;
    let page_url = html::meta(body, "dg:plink").unwrap_or_else(|| page.to_owned());
    let mut info = SourceInfo::new(known.site, page_url);
    info.files = html::tags(content, "img")
        .into_iter()
        .filter_map(|t| t.attr("src").map(str::to_owned))
        .filter_map(|src| {
            let parsed = moekura_core::sites::parse(&src)?;
            matches!(parsed.site.key, "tistory" | "kakao").then(|| parsed.file_url.unwrap_or(src))
        })
        .collect();
    info.artist_name =
        html::find(body, "cite", |t| t.has_class("by_blog")).map(|(_, inner)| html_to_text(inner));
    info.title = html::find(body, "h3", |t| t.has_class("tit_blogview"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.tags = tags_named(
        html::tags(body, "a")
            .into_iter()
            .filter(|a| a.attr("rel") == Some("tag"))
            .filter_map(|a| html::label(body, &a))
            .collect::<Vec<_>>(),
    );
    info.description = html_to_text(content);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts() {
        let page = "https://primemeeting.tistory.com/25";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<meta property="dg:plink" content="https://primemeeting.tistory.com/25">
            <h3 class="tit_blogview">Art</h3><cite class="by_blog">Prime</cite>
            <div class="blogview_content"><p>Hi</p><img src="https://img1.daumcdn.net/thumb/R1280x0/?fname=https%3A%2F%2Fblog.kakaocdn.net%2Fdn%2Fa%2Fimg.jpg"><img src="https://pbs.twimg.com/media/x.jpg"></div>
            <div class="list_tag"><a rel="tag" href="/tag/cat">cat</a></div>"#;
        let info = post_info(&known, page, body).unwrap();
        assert_eq!(info.files, ["https://blog.kakaocdn.net/dn/a/img.jpg"]);
        assert_eq!(info.tags[0].name, "cat");
        assert_eq!(info.description, "Hi");
    }
}
