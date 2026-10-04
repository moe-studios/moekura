//! Posts on X (Twitter): `x.com/<user>/status/<id>` (or twitter.com,
//! with `/photo/<n>`). Read through an FxEmbed instance's API
//! (`[sources.x]`), which needs no account and sees age-restricted posts;
//! failing that, from X itself as the account `[sources.logins."x.com"]`
//! gives, if any; and last from the embed API that shows posts on other
//! sites, which only sees public ones. Images are asked for at full size.

use serde_json::Value;
use time::OffsetDateTime;
use url::Url;

use super::{Http, SourceInfo, SourceTag, text_of};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub user: String,
    pub id: u64,
    /// `/photo/<n>`'s picture, 0-based.
    pub page: usize,
}

pub(super) fn target(url: &Url) -> Option<Target> {
    let host = url
        .host_str()?
        .trim_start_matches("www.")
        .trim_start_matches("mobile.");
    if !matches!(
        host,
        "x.com" | "twitter.com" | "fxtwitter.com" | "vxtwitter.com" | "fixupx.com"
    ) {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [user, "status", id, rest @ ..] => Some(Target {
            user: (*user).to_owned(),
            id: id.parse().ok()?,
            page: match rest {
                ["photo", n, ..] => n.parse::<usize>().ok()?.saturating_sub(1),
                _ => 0,
            },
        }),
        _ => None,
    }
}

/// What uploaders are told when a post couldn't be read.
pub(super) const UNREAD: &str = "X didn't give this post. It may not exist, may be hidden \
     (deleted, or from a protected account) or may be age-restricted.";

/// The X web app's own API key, which logged-in requests to its API carry.
const WEB_APP_BEARER: &str = "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs\
     %3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";
/// X's API query for one post; X changes these ids now and then.
const POST_QUERY: &str = "https://x.com/i/api/graphql/2ICDjqPd81tulZcYrtpTuQ/TweetResultByRestId";

/// The embed API's token for a post: its id ÷ 10¹⁵ × π, in base 36,
/// without zeros and the point.
fn token(id: u64) -> String {
    let value = (id as f64 / 1e15) * std::f64::consts::PI;
    let mut whole = value.trunc() as u64;
    let mut fraction = value.fract();
    let digit = |d: u64| char::from_digit(d as u32, 36).unwrap_or('0');
    let mut int_part = String::new();
    loop {
        int_part.insert(0, digit(whole % 36));
        whole /= 36;
        if whole == 0 {
            break;
        }
    }
    let mut frac_part = String::new();
    for _ in 0..11 {
        fraction *= 36.0;
        let d = fraction.trunc() as u64;
        frac_part.push(digit(d));
        fraction -= d as f64;
    }
    format!("{int_part}{frac_part}").replace('0', "")
}

/// What a post says, however it was read.
#[derive(Debug, Default)]
struct Post {
    screen_name: String,
    name: Option<String>,
    user_id: Option<String>,
    text: String,
    files: Vec<String>,
    hashtags: Vec<String>,
    published_at: Option<OffsetDateTime>,
}

impl Post {
    fn into_info(mut self, target: &Target) -> SourceInfo {
        if target.page > 0 && target.page < self.files.len() {
            let chosen = self.files.remove(target.page);
            self.files.insert(0, chosen);
        }
        let screen_name = self.screen_name;
        let mut profile_urls = vec![format!("https://twitter.com/{screen_name}")];
        if let Some(id) = self.user_id {
            profile_urls.push(format!("https://twitter.com/intent/user?user_id={id}"));
        }
        // The text, without the link to the post's own media at the end.
        let text = self
            .text
            .rsplit_once(" https://t.co/")
            .filter(|(_, link)| !link.contains(' '))
            .map_or(self.text.as_str(), |(before, _)| before)
            .trim()
            .to_owned();
        SourceInfo {
            site: moekura_core::sites::TWITTER.name,
            page_url: format!("https://twitter.com/{screen_name}/status/{}", target.id),
            files: self.files,
            headers: Vec::new(),
            artist_name: self.name,
            artist_account: Some(screen_name),
            profile_urls,
            tags: self
                .hashtags
                .into_iter()
                .map(|name| SourceTag {
                    name,
                    translation: None,
                })
                .collect(),
            title: String::new(),
            description: text,
            ugoira_frames: None,
            published_at: self.published_at,
            updated_at: None,
        }
    }
}

/// The best MP4 of a video or GIF, from `variants` (each with a `url`,
/// a `bitrate` and a `content_type` or `container`).
fn best_mp4(variants: &Value) -> Option<String> {
    variants
        .as_array()?
        .iter()
        .filter(|v| v["content_type"] == "video/mp4" || v["container"] == "mp4")
        .max_by_key(|v| v["bitrate"].as_u64().unwrap_or(0))?["url"]
        .as_str()
        .map(str::to_owned)
}

/// An FxEmbed API's answer (`{"code": 200, "tweet": {…}}`).
fn parse_fxembed(answer: &Value) -> Result<Post, String> {
    let post = &answer["tweet"];
    if answer["code"] != 200 {
        let why = post["reason"]
            .as_str()
            .or_else(|| answer["message"].as_str())
            .unwrap_or("no post");
        return Err(format!("answered {why}"));
    }
    let author = &post["author"];
    let files = post["media"]["all"]
        .as_array()
        .map(|media| {
            media
                .iter()
                .filter_map(|m| match m["type"].as_str()? {
                    "photo" => m["url"].as_str().map(str::to_owned),
                    // A GIF's `url` is its still; its MP4 is a format.
                    "video" | "gif" => best_mp4(&m["formats"])
                        .or_else(|| (m["type"] == "video").then(|| text_of(&m["url"]))),
                    _ => None,
                })
                .filter(|url| !url.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let hashtags = post["raw_text"]["facets"]
        .as_array()
        .map(|facets| {
            facets
                .iter()
                .filter(|f| f["type"] == "hashtag")
                .filter_map(|f| f["original"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(Post {
        screen_name: author["screen_name"]
            .as_str()
            .ok_or("answered without an author")?
            .to_owned(),
        name: author["name"].as_str().map(str::to_owned),
        user_id: author["id"].as_str().map(str::to_owned),
        text: text_of(&post["text"]),
        files,
        hashtags,
        published_at: post["created_timestamp"]
            .as_i64()
            .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()),
    })
}

/// A post as X's own APIs give it: `media` is its list of photos and
/// videos (`mediaDetails` or `extended_entities.media`), and `user` its
/// author.
fn parse_x(post: &Value, media: &Value, text: &Value) -> Option<Post> {
    let user = &post["user"];
    let files = media
        .as_array()
        .map(|media| {
            media
                .iter()
                .filter_map(|m| match m["type"].as_str() {
                    Some("photo") => m["media_url_https"]
                        .as_str()
                        .map(|u| format!("{u}?name=orig")),
                    _ => best_mp4(&m["video_info"]["variants"]),
                })
                .collect()
        })
        .unwrap_or_default();
    let hashtags = post["entities"]["hashtags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|t| t["text"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(Post {
        screen_name: user["screen_name"].as_str()?.to_owned(),
        name: user["name"].as_str().map(str::to_owned),
        user_id: user["id_str"].as_str().map(str::to_owned),
        text: super::decode_entities(&text_of(text)),
        files,
        hashtags,
        published_at: post["created_at"].as_str().and_then(super::date),
    })
}

/// The embed API's answer.
fn parse_embed(post: &Value) -> Option<Post> {
    parse_x(post, &post["mediaDetails"], &post["text"])
}

/// X's API's answer to [`POST_QUERY`].
fn parse_logged_in(answer: &Value, id: u64) -> Result<Post, String> {
    let mut result = &answer["data"]["tweetResult"]["result"];
    match result["__typename"].as_str() {
        Some("TweetWithVisibilityResults") => result = &result["tweet"],
        Some("TweetUnavailable") => {
            let why = result["reason"].as_str().unwrap_or("unavailable");
            return Err(format!("X says the post is {why}"));
        }
        Some("TweetTombstone") => return Err("X says the post is gone".into()),
        _ => {}
    }
    let legacy = &result["legacy"];
    if !legacy.is_object() {
        let errors = answer["errors"][0]["message"].as_str();
        return Err(errors.unwrap_or("X answered without the post").to_owned());
    }
    // The author's names have been moving from `legacy` to `core`.
    let user = &result["core"]["user_results"]["result"];
    let names = |key: &str| {
        user["core"][key]
            .as_str()
            .or_else(|| user["legacy"][key].as_str())
            .map(str::to_owned)
    };
    let post = serde_json::json!({
        "user": {
            "screen_name": names("screen_name"),
            "name": names("name"),
            "id_str": user["rest_id"],
        },
        "entities": legacy["entities"],
    });
    let mut found = parse_x(
        &post,
        &legacy["extended_entities"]["media"],
        &legacy["full_text"],
    )
    .ok_or("X answered without the post's author")?;
    found.published_at = snowflake_time(id);
    Ok(found)
}

/// When a post was made, from its id (ids have said since late 2010).
fn snowflake_time(id: u64) -> Option<OffsetDateTime> {
    const EPOCH_MS: i128 = 1_288_834_974_657;
    if id < 1 << 32 {
        return None;
    }
    let ms = i128::from(id >> 22) + EPOCH_MS;
    OffsetDateTime::from_unix_timestamp_nanos(ms * 1_000_000).ok()
}

/// The `ct0` cookie of a cookie header, which X wants repeated as a header.
fn csrf_token(cookie: &str) -> Option<&str> {
    cookie
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == "ct0")
        .map(|(_, value)| value)
}

async fn from_fxembed(http: &Http<'_>, api: &str, target: &Target) -> Result<Post, String> {
    let url = format!("{api}/status/{}", target.id);
    parse_fxembed(&http.json(&url, &[]).await?)
}

async fn from_x(http: &Http<'_>, target: &Target) -> Result<Post, String> {
    let login = http.logins.login_for("x.com").ok_or("no x.com login")?;
    let csrf = csrf_token(&login.cookie).ok_or("the x.com login's cookie lacks ct0")?;
    let variables = serde_json::json!({
        "tweetId": target.id.to_string(),
        "withCommunity": false,
        "includePromotedContent": false,
        "withVoice": false,
    });
    let features = serde_json::json!({
        "creator_subscriptions_tweet_preview_api_enabled": true,
        "tweetypie_unmention_optimization_enabled": true,
        "responsive_web_edit_tweet_api_enabled": true,
        "graphql_is_translatable_rweb_tweet_is_translatable_enabled": true,
        "view_counts_everywhere_api_enabled": true,
        "longform_notetweets_consumption_enabled": true,
        "responsive_web_twitter_article_tweet_consumption_enabled": false,
        "tweet_awards_web_tipping_enabled": false,
        "freedom_of_speech_not_reach_fetch_enabled": true,
        "standardized_nudges_misinfo": true,
        "tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled": true,
        "longform_notetweets_rich_text_read_enabled": true,
        "longform_notetweets_inline_media_enabled": true,
        "responsive_web_graphql_exclude_directive_enabled": true,
        "verified_phone_label_enabled": false,
        "responsive_web_media_download_video_enabled": false,
        "responsive_web_graphql_skip_user_profile_image_extensions_enabled": false,
        "responsive_web_graphql_timeline_navigation_enabled": true,
        "responsive_web_enhance_cards_enabled": false,
    });
    let mut url = Url::parse(POST_QUERY).map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("variables", &variables.to_string())
        .append_pair("features", &features.to_string());
    let bearer = format!("Bearer {WEB_APP_BEARER}");
    let headers = [
        ("Authorization", bearer.as_str()),
        ("x-csrf-token", csrf),
        ("x-twitter-auth-type", "OAuth2Session"),
        ("x-twitter-active-user", "yes"),
    ];
    parse_logged_in(&http.json(url.as_str(), &headers).await?, target.id)
}

async fn from_embed(http: &Http<'_>, target: &Target) -> Result<Post, String> {
    let url = format!(
        "https://cdn.syndication.twimg.com/tweet-result?id={}&token={}&lang=en",
        target.id,
        token(target.id)
    );
    parse_embed(&http.json(&url, &[]).await?).ok_or_else(|| "no such post, or it's hidden".into())
}

pub(super) async fn fetch(http: &Http<'_>, target: &Target) -> Result<SourceInfo, String> {
    let mut failed = Vec::new();
    let api = http.logins.x.fxembed_api.trim().trim_end_matches('/');
    if !api.is_empty() {
        match from_fxembed(http, api, target).await {
            Ok(post) => return Ok(post.into_info(target)),
            Err(error) => failed.push(format!("{api}: {error}")),
        }
    }
    if http.has_login("x.com") {
        match from_x(http, target).await {
            Ok(post) => return Ok(post.into_info(target)),
            Err(error) => failed.push(format!("logged in to X: {error}")),
        }
    }
    match from_embed(http, target).await {
        Ok(post) => Ok(post.into_info(target)),
        Err(error) => {
            failed.push(format!("embed API: {error}"));
            Err(format!("X: {}", failed.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn urls() {
        let t = |u: &str| target(&Url::parse(u).unwrap());
        assert_eq!(
            t("https://x.com/artist/status/17/photo/2"),
            Some(Target {
                user: "artist".into(),
                id: 17,
                page: 1
            })
        );
        assert!(t("https://twitter.com/artist").is_none());
        assert!(t("https://example.com/a/status/1").is_none());
    }

    #[test]
    fn tokens_have_no_zeros() {
        let token = token(1_800_000_000_000_000_000);
        assert!(!token.is_empty() && !token.contains('0') && !token.contains('.'));
    }

    fn second_photo() -> Target {
        Target {
            user: "x".into(),
            id: 5,
            page: 1,
        }
    }

    #[test]
    fn embed_posts() {
        let post = json!({
            "text": "New drawing &amp; more #cat https://t.co/abc",
            "user": { "screen_name": "artist", "name": "Artist", "id_str": "42" },
            "entities": { "hashtags": [{ "text": "cat" }] },
            "mediaDetails": [
                { "type": "photo", "media_url_https": "https://pbs.twimg.com/media/a.jpg" },
                { "type": "photo", "media_url_https": "https://pbs.twimg.com/media/b.jpg" }
            ]
        });
        let info = parse_embed(&post).unwrap().into_info(&second_photo());
        assert_eq!(info.files[0], "https://pbs.twimg.com/media/b.jpg?name=orig");
        assert_eq!(info.description, "New drawing & more #cat");
        assert_eq!(info.page_url, "https://twitter.com/artist/status/5");
        assert_eq!(
            info.profile_urls[1],
            "https://twitter.com/intent/user?user_id=42"
        );
        assert_eq!(info.tags[0].name, "cat");
    }

    #[test]
    fn fxembed_posts() {
        let answer = json!({
            "code": 200,
            "message": "OK",
            "tweet": {
                "id": "5",
                "text": "New drawing & more #cat",
                "raw_text": {
                    "text": "New drawing & more #cat https://t.co/abc",
                    "facets": [
                        { "type": "hashtag", "original": "cat" },
                        { "type": "media", "original": "https://t.co/abc" }
                    ]
                },
                "author": { "screen_name": "artist", "name": "Artist", "id": "42" },
                "created_timestamp": 1_513_770_067,
                "media": { "all": [
                    {
                        "type": "photo",
                        "url": "https://pbs.twimg.com/media/a.jpg?name=orig"
                    },
                    {
                        "type": "video",
                        "url": "https://video.twimg.com/low.mp4",
                        "formats": [
                            { "container": "m3u8", "url": "https://video.twimg.com/v.m3u8" },
                            { "container": "mp4", "bitrate": 832_000, "url": "https://video.twimg.com/low.mp4" },
                            { "container": "mp4", "bitrate": 2_176_000, "url": "https://video.twimg.com/high.mp4" }
                        ]
                    },
                    {
                        "type": "gif",
                        "url": "https://pbs.twimg.com/tweet_video_thumb/g.jpg",
                        "formats": [{ "container": "mp4", "bitrate": 0, "url": "https://video.twimg.com/g.mp4" }]
                    }
                ] }
            }
        });
        let info = parse_fxembed(&answer).unwrap().into_info(&second_photo());
        assert_eq!(
            info.files,
            [
                "https://video.twimg.com/high.mp4",
                "https://pbs.twimg.com/media/a.jpg?name=orig",
                "https://video.twimg.com/g.mp4",
            ]
        );
        assert_eq!(info.description, "New drawing & more #cat");
        assert_eq!(info.artist_account.as_deref(), Some("artist"));
        assert_eq!(info.tags[0].name, "cat");
        assert_eq!(info.published_at.unwrap().unix_timestamp(), 1_513_770_067);
    }

    #[test]
    fn fxembed_says_why_a_post_is_missing() {
        let answer = json!({
            "code": 404,
            "message": "NOT_FOUND",
            "tweet": { "type": "tombstone", "reason": "unavailable" }
        });
        assert_eq!(parse_fxembed(&answer).unwrap_err(), "answered unavailable");
    }

    #[test]
    fn logged_in_posts() {
        let answer = json!({ "data": { "tweetResult": { "result": {
            "__typename": "TweetWithVisibilityResults",
            "tweet": {
                "core": { "user_results": { "result": {
                    "rest_id": "42",
                    "core": { "screen_name": "artist", "name": "Artist" },
                    "legacy": {}
                } } },
                "legacy": {
                    "full_text": "Lewd &amp; more #cat https://t.co/abc",
                    "entities": { "hashtags": [{ "text": "cat" }] },
                    "extended_entities": { "media": [
                        { "type": "photo", "media_url_https": "https://pbs.twimg.com/media/a.jpg" }
                    ] }
                }
            }
        } } } });
        let id = 943_446_161_586_733_056;
        let post = parse_logged_in(&answer, id).unwrap();
        assert_eq!(post.published_at.unwrap().unix_timestamp(), 1_513_770_067);
        let info = post.into_info(&second_photo());
        assert_eq!(info.files, ["https://pbs.twimg.com/media/a.jpg?name=orig"]);
        assert_eq!(info.description, "Lewd & more #cat");
        assert_eq!(info.artist_name.as_deref(), Some("Artist"));
        assert_eq!(
            info.profile_urls[1],
            "https://twitter.com/intent/user?user_id=42"
        );

        let hidden = json!({ "data": { "tweetResult": { "result": {
            "__typename": "TweetUnavailable",
            "reason": "NsfwLoggedOut"
        } } } });
        assert_eq!(
            parse_logged_in(&hidden, id).unwrap_err(),
            "X says the post is NsfwLoggedOut"
        );
    }

    #[test]
    fn csrf_tokens() {
        assert_eq!(csrf_token("auth_token=a; ct0=b"), Some("b"));
        assert_eq!(csrf_token("auth_token=a"), None);
    }
}
