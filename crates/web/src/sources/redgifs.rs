//! RedGIFs, from its API (with a temporary token it hands out): the GIF
//! (or a gallery's), the user, tags and niches.

use moekura_core::sites::SourceUrl;
use serde_json::Value;

use super::{Http, SourceInfo, strings, tags_named, text_of};

pub(super) async fn fetch(
    http: &Http<'_>,
    known: &SourceUrl,
    page: &str,
) -> Result<SourceInfo, String> {
    let id = page.rsplit('/').next().unwrap_or_default();
    let token = http
        .json("https://api.redgifs.com/v2/auth/temporary", &[])
        .await?;
    let bearer = format!("Bearer {}", text_of(&token["token"]));
    let headers = [("Authorization", bearer.as_str())];
    let answer = http
        .json(
            &format!("https://api.redgifs.com/v2/gifs/{id}?users=yes&niches=yes"),
            &headers,
        )
        .await?;
    let gallery = match answer["gif"]["gallery"].as_str() {
        Some(gallery) => http
            .json(
                &format!("https://api.redgifs.com/v2/gallery/{gallery}"),
                &headers,
            )
            .await
            .ok(),
        None => None,
    };
    gif_info(known, page, &answer, gallery.as_ref()).ok_or_else(|| "RedGIFs: no such GIF".into())
}

fn gif_info(
    known: &SourceUrl,
    page: &str,
    answer: &Value,
    gallery: Option<&Value>,
) -> Option<SourceInfo> {
    let gif = &answer["gif"];
    let file = gif["urls"]["hd"].as_str()?;
    let mut info = SourceInfo::new(known.site, page);
    // The gallery's first GIF (its cover) stands for all of it.
    let gifs = gallery.and_then(|g| g["gifs"].as_array());
    let cover = gifs.and_then(|g| g.first());
    info.files = match (gifs, cover) {
        (Some(gifs), Some(cover)) if cover["id"] == gif["id"] => gifs
            .iter()
            .filter_map(|g| g["urls"]["hd"].as_str().map(str::to_owned))
            .collect(),
        _ => vec![file.to_owned()],
    };
    let user = &answer["user"];
    info.artist_name = user["name"].as_str().map(str::to_owned);
    if let Some(name) = user["username"].as_str() {
        info.artist_account = Some(name.to_owned());
        info.profile_urls.push(format!(
            "https://www.redgifs.com/users/{}",
            name.to_ascii_lowercase()
        ));
    }
    let mut tags = strings(&gif["tags"], None);
    tags.extend(strings(&gif["niches"], None));
    info.tags = tags_named(tags);
    info.description = cover
        .and_then(|c| c["description"].as_str())
        .or_else(|| gif["description"].as_str())
        .unwrap_or_default()
        .to_owned();
    Some(info)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn gifs() {
        let page = "https://www.redgifs.com/watch/thunderousverifiablescoter";
        let known = moekura_core::sites::parse(page).unwrap();
        let answer = json!({
            "gif": { "id": "thunderousverifiablescoter", "urls": { "hd": "https://media.redgifs.com/Thunderous.mp4" }, "tags": ["Art"], "niches": ["anime"] },
            "user": { "username": "LazyProcrastinator", "name": "Lazy" }
        });
        let info = gif_info(&known, page, &answer, None).unwrap();
        assert_eq!(info.files, ["https://media.redgifs.com/Thunderous.mp4"]);
        assert_eq!(
            info.profile_urls,
            ["https://www.redgifs.com/users/lazyprocrastinator"]
        );
        assert_eq!(info.tags.len(), 2);
    }
}
