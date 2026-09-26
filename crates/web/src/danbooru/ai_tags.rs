//! The tagger's suggestions in Danbooru's shape: `/ai_tags.json`. Each
//! post has one media asset, numbered like the post.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::tag_suggestions::{self, Listed, SuggestionFilter};
use serde::{Deserialize, Serialize};

use super::{ListParams, json};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::posts::visibility;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/ai_tags", get(list))
}

#[derive(Debug, Serialize)]
struct AiTag {
    media_asset_id: i64,
    post_id: i64,
    tag_id: i32,
    /// Confidence, 0 to 100.
    score: u32,
    is_posted: bool,
    tag: AiTagTag,
}

#[derive(Debug, Serialize)]
struct AiTagTag {
    id: i32,
    name: String,
    category: i16,
    post_count: i32,
}

impl From<Listed> for AiTag {
    fn from(s: Listed) -> Self {
        Self {
            media_asset_id: s.post_id,
            post_id: s.post_id,
            tag_id: s.tag_id,
            score: (s.confidence * 100.0).round().clamp(0.0, 100.0) as u32,
            is_posted: s.posted,
            tag: AiTagTag {
                id: s.tag_id,
                name: s.name,
                category: s.category_id,
                post_count: s.post_count,
            },
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct AiTagParams {
    #[serde(rename = "search[media_asset_id]", default)]
    media_asset_id: String,
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(rename = "search[tag_id]", default)]
    tag_id: String,
    #[serde(rename = "search[tag_name]", default)]
    tag_name: String,
    #[serde(rename = "search[is_posted]", default)]
    is_posted: String,
    #[serde(rename = "search[score]", default)]
    score: String,
    #[serde(flatten)]
    list: ListParams,
}

/// Numbers from a list separated by spaces or commas.
fn numbers<T: std::str::FromStr>(value: &str) -> Vec<T> {
    value
        .split([' ', ','])
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

/// A score range, inclusive, from `50`, `>50`, `>=50`, `<50`, `<=50` or
/// `50..90`.
fn score_range(value: &str) -> Result<(i32, i32), AppError> {
    let value = value.trim();
    let number = |s: &str| {
        s.trim().parse::<i32>().map_err(|_| {
            AppError::BadRequest("`search[score]` must be like 50, >=50 or 50..90".into())
        })
    };
    Ok(if value.is_empty() {
        (0, 100)
    } else if let Some(n) = value.strip_prefix(">=") {
        (number(n)?, 100)
    } else if let Some(n) = value.strip_prefix('>') {
        (number(n)? + 1, 100)
    } else if let Some(n) = value.strip_prefix("<=") {
        (0, number(n)?)
    } else if let Some(n) = value.strip_prefix('<') {
        (0, number(n)? - 1)
    } else if let Some((low, high)) = value.split_once("..") {
        (number(low)?, number(high)?)
    } else {
        let n = number(value)?;
        (n, n)
    })
}

async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<AiTagParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let mut post_ids: Vec<i64> = numbers(&params.post_id);
    post_ids.extend(numbers::<i64>(&params.media_asset_id));
    let tag_ids: Vec<i32> = numbers(&params.tag_id);
    let tag_names: Vec<String> = params
        .tag_name
        .split([' ', ','])
        .map(moekura_core::tags::normalize)
        .filter(|name| !name.is_empty())
        .collect();
    let posted = match params.is_posted.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    let seen = visibility(&current);
    let statuses: Vec<&str> = seen.statuses.iter().map(|s| s.as_str()).collect();
    let filter = SuggestionFilter {
        post_ids: &post_ids,
        tag_ids: &tag_ids,
        tag_names: &tag_names,
        posted,
        score: score_range(&params.score)?,
        statuses: &statuses,
        viewer: seen.viewer,
    };
    let limit = params.list.limit(1000);
    let page = params
        .list
        .page
        .trim()
        .parse::<u32>()
        .unwrap_or(1)
        .clamp(1, 1000);
    let found = tag_suggestions::list(
        db,
        &filter,
        i64::from(page - 1) * i64::from(limit),
        i64::from(limit),
    )
    .await?;
    json(
        found.into_iter().map(AiTag::from).collect::<Vec<_>>(),
        &params.list.only,
    )
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;
    use moekura_core::posts::Rating;
    use moekura_db::tag_suggestions::NewResult;
    use moekura_db::tags::{self, WantedTag};
    use serde_json::Value;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, fixture, session_for, test_state};

    #[test]
    fn reads_score_ranges() {
        assert_eq!(score_range("").unwrap(), (0, 100));
        assert_eq!(score_range("50").unwrap(), (50, 50));
        assert_eq!(score_range(">50").unwrap(), (51, 100));
        assert_eq!(score_range(">=50").unwrap(), (50, 100));
        assert_eq!(score_range("<=50").unwrap(), (0, 50));
        assert_eq!(score_range("20..40").unwrap(), (20, 40));
        assert!(score_range("lots").is_err());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn lists_suggestions(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            crate::upload::routes(max).merge(crate::danbooru::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let fields = vec![("rating", "g".to_owned()), ("tags", "cat".to_owned())];
        let response = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &fields,
                Some(("a.png", &fixture::png(8, 8))),
            )
            .await;
        let post_id: i64 = response.location.unwrap()["/posts/".len()..]
            .parse()
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let wanted = ["cat", "whiskers"].map(|name| WantedTag {
            name,
            category_id: None,
        });
        let found = tags::ensure(&mut conn, &wanted, false).await.unwrap();
        let id = |name: &str| found.iter().find(|t| t.name == name).unwrap().id;
        moekura_db::tag_suggestions::save(
            &mut conn,
            &NewResult {
                post_id,
                model: "m",
                rating: Rating::General,
                rating_confidence: 0.9,
                suggestions: &[(id("cat"), 0.97), (id("whiskers"), 0.6)],
            },
        )
        .await
        .unwrap();

        let get = async |query: &str| -> Value {
            let query = query
                .replace('[', "%5B")
                .replace(']', "%5D")
                .replace('>', "%3E");
            let response = app.get(&format!("/ai_tags.json{query}"), None).await;
            serde_json::from_str(&response.body).unwrap()
        };
        let all = get("").await;
        assert_eq!(all.as_array().unwrap().len(), 2);
        assert_eq!(all[0]["media_asset_id"], post_id);
        assert_eq!(all[0]["tag"]["name"], "cat");
        assert_eq!(all[0]["score"], 97);
        assert_eq!(all[0]["is_posted"], true);

        let unposted = get("?search[is_posted]=false").await;
        assert_eq!(unposted.as_array().unwrap().len(), 1);
        assert_eq!(unposted[0]["tag"]["name"], "whiskers");
        assert_eq!(get("?search[score]=>90").await.as_array().unwrap().len(), 1);
        assert_eq!(
            get("?search[tag_name]=Whiskers&only=score").await,
            serde_json::json!([{ "score": 60 }])
        );
        assert_eq!(
            get(&format!("?search[post_id]={}", post_id + 1)).await,
            serde_json::json!([])
        );
    }
}
