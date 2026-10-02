//! DC Inside gallery posts, from their pages: the post's images (not its
//! stickers), the writer and the text.

use moekura_core::sites::SourceUrl;

use super::{Http, SourceInfo, html, html_to_text};

const REFERER: &str = "https://gall.dcinside.com/";

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let body = http.page(page, &[]).await?;
    post_info(known, page, &body).ok_or_else(|| "DC Inside: no post in the page".into())
}

fn post_info(known: &SourceUrl, page: &str, body: &str) -> Option<SourceInfo> {
    let (_, content) = html::find(body, "div", |t| t.has_class("write_div"))?;
    let mut info = SourceInfo::new(known.site, page);
    info.headers = vec![("Referer", REFERER.to_owned())];
    info.files = html::tags(content, "img")
        .into_iter()
        .chain(html::tags(content, "video"))
        .filter(|t| !t.has_class("written_dccon"))
        .filter_map(|t| {
            // imgPop('<original>') in onclick names the original.
            let popup = t
                .attr("onclick")
                .and_then(|js| html::between(js, "imgPop('", "'"))
                .map(str::to_owned);
            popup.or_else(|| {
                t.attr("data-original")
                    .or_else(|| t.attr("data-src"))
                    .or_else(|| t.attr("src"))
                    .map(str::to_owned)
            })
        })
        .filter(|src| src.contains("dcinside.co") && src.contains("viewimage"))
        .map(|src| {
            moekura_core::sites::parse(&src)
                .and_then(|u| u.file_url)
                .unwrap_or(src)
        })
        .collect();
    if let Some(writer) = html::tags(body, "div")
        .into_iter()
        .find(|t| t.has_class("gall_writer"))
    {
        info.artist_name = writer.attr("data-nick").map(str::to_owned);
        if let Some(id) = writer.attr("data-uid").filter(|u| !u.is_empty()) {
            info.artist_account = Some(id.to_owned());
            info.profile_urls
                .push(format!("https://gallog.dcinside.com/{id}"));
        }
    }
    info.title = html::find(body, "span", |t| t.has_class("title_subject"))
        .map(|(_, inner)| html_to_text(inner))
        .unwrap_or_default();
    info.description = html_to_text(content);
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts() {
        let page = "https://gall.dcinside.com/mgallery/board/view/?id=projectmx&no=11076518";
        let known = moekura_core::sites::parse(page).unwrap();
        let body = r#"<div class="gall_writer ub-writer" data-nick="Neko" data-uid="neko01"></div>
            <span class="title_subject">Art</span>
            <div class="write_div"><p>Hi</p><img src="https://dcimg8.dcinside.co.kr/viewimage.php?id=3d&no=24b0" onclick="javascript:imgPop('https://image.dcinside.com/viewimagePop.php?no=24b0','x')">
            <img class="written_dccon" src="https://dcimg5.dcinside.com/dccon.php?no=1"></div>"#;
        let info = post_info(&known, page, body).unwrap();
        assert_eq!(
            info.files,
            ["https://image.dcinside.com/viewimage.php?no=24b0"]
        );
        assert_eq!(info.profile_urls, ["https://gallog.dcinside.com/neko01"]);
        assert_eq!(info.title, "Art");
    }
}
