//! Inkbunny submissions, from its API. Without a login (a session id in
//! `[sources.logins."inkbunny.net"]`'s `query`, `sid = "…"`), a guest
//! session sees what visitors see.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, html_to_text, id_of, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = page.rsplit('/').next().unwrap_or_default();
    let sid = if http.has_login("inkbunny.net") {
        String::new()
    } else {
        let guest = http
            .json(
                "https://inkbunny.net/api_login.php?username=guest&password=",
                &[],
            )
            .await?;
        format!("&sid={}", text_of(&guest["sid"]))
    };
    let answer = http
        .json(
            &format!(
                "https://inkbunny.net/api_submissions.php?show_description_bbcode_parsed=yes&submission_ids={id}{sid}"
            ),
            &[],
        )
        .await?;
    submission_info(known, page, &answer["submissions"][0])
        .ok_or_else(|| "Inkbunny: no such submission".into())
}

fn submission_info(known: &SourceUrl, page: &str, submission: &Value) -> Option<SourceInfo> {
    let name = submission["username"].as_str()?;
    let mut info = SourceInfo::new(known.site, page);
    info.files = strings(&submission["files"], Some("file_url_full"));
    info.artist_name = Some(name.to_owned());
    info.artist_account = Some(name.to_owned());
    info.profile_urls
        .push(format!("https://inkbunny.net/{name}"));
    if let Some(id) = id_of(&submission["user_id"]) {
        info.profile_urls
            .push(format!("https://inkbunny.net/user.php?user_id={id}"));
    }
    info.tags = tags_named(strings(&submission["keywords"], Some("keyword_name")));
    info.title = text_of(&submission["title"]);
    info.description = html_to_text(&text_of(&submission["description_bbcode_parsed"]));
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn submissions() {
        let page = "https://inkbunny.net/s/3200751";
        let known = moekura_core::sites::parse(page).unwrap();
        let submission = json!({
            "username": "DAGASI", "user_id": "152800", "title": "Cat", "description_bbcode_parsed": "Hi<br />there",
            "files": [{ "file_url_full": "https://us.ib.metapix.net/files/full/4/a.png" }],
            "keywords": [{ "keyword_name": "cat" }]
        });
        let info = submission_info(&known, page, &submission).unwrap();
        assert_eq!(info.files, ["https://us.ib.metapix.net/files/full/4/a.png"]);
        assert_eq!(
            info.profile_urls[1],
            "https://inkbunny.net/user.php?user_id=152800"
        );
        assert_eq!(info.description, "Hi\nthere");
    }
}
