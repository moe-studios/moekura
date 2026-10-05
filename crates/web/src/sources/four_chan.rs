//! 4chan threads and posts, from its read-only JSON API.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html_to_text, key, number, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    // https://boards.4chan.org/<board>/thread/<id>[#p<post>]
    let (thread_page, post) = page
        .split_once("#p")
        .map_or((page, None), |(t, p)| (t, Some(p)));
    let parts: Vec<&str> = thread_page.split('/').collect();
    let [.., board, "thread", thread] = parts.as_slice() else {
        return Err("4chan: not a thread".into());
    };
    let (board, thread) = (key(board)?, number(thread)?);
    let api = http
        .json(
            &format!("https://a.4cdn.org/{board}/thread/{thread}.json"),
            &[],
        )
        .await?;
    thread_info(known, page, board, &api, post).ok_or_else(|| "4chan: no such post".into())
}

fn file_of(board: &str, post: &Value) -> Option<String> {
    let tim = post["tim"].as_i64()?;
    let ext = post["ext"].as_str()?;
    Some(format!("https://i.4cdn.org/{board}/{tim}{ext}"))
}

fn thread_info(
    known: &SourceUrl,
    page: &str,
    board: &str,
    api: &Value,
    post: Option<&str>,
) -> Option<SourceInfo> {
    let posts = api["posts"].as_array()?;
    let mut info = SourceInfo::new(known.site, page);
    match post.and_then(|p| p.parse::<i64>().ok()) {
        Some(no) => {
            let post = posts.iter().find(|p| p["no"].as_i64() == Some(no))?;
            info.files = file_of(board, post).into_iter().collect();
            info.title = format!(
                "{}{} {} No.{no}",
                text_of(&post["name"]),
                text_of(&post["trip"]),
                text_of(&post["now"])
            );
            info.description = html_to_text(&text_of(&post["com"]));
        }
        None => {
            info.files = posts.iter().filter_map(|p| file_of(board, p)).collect();
            info.title = text_of(&posts.first()?["sub"]);
            info.description = html_to_text(&text_of(&posts.first()?["com"]));
        }
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn threads_and_posts() {
        let page = "https://boards.4chan.org/vt/thread/37293562#p37294005";
        let known = moekura_core::sites::parse(page).unwrap();
        let api = json!({ "posts": [
            { "no": 37293562, "sub": "Thread", "com": "OP", "tim": 1, "ext": ".png" },
            { "no": 37294005, "name": "Anonymous", "now": "11/17/22", "com": "Hi<br>there", "tim": 2, "ext": ".webm" }
        ]});
        let info = thread_info(&known, page, "vt", &api, Some("37294005")).unwrap();
        assert_eq!(info.files, ["https://i.4cdn.org/vt/2.webm"]);
        assert_eq!(info.description, "Hi\nthere");
        let info = thread_info(&known, page, "vt", &api, None).unwrap();
        assert_eq!(info.files.len(), 2);
    }
}
