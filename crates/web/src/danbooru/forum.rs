//! `/forum_topics.json`, `/forum_posts.json` and `/forum_post_votes.json`
//! (your own votes), with single topics and posts, and posting.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::forum::{self, Post, PostFilter, Topic, TopicFilter};
use serde::{Deserialize, Serialize};

use super::tags::window;
use super::{Fields, ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::forum::moderates;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/forum_topics", get(topics).post(create_topic))
        .route("/forum_topics/{id}", get(topic))
        .route("/forum_posts", get(posts).post(create_post))
        .route("/forum_posts/{id}", get(post))
        .route("/forum_post_votes", get(votes))
}

#[derive(Debug, Serialize)]
struct DanbooruTopic {
    id: i64,
    creator_id: Option<i64>,
    updater_id: Option<i64>,
    title: String,
    response_count: i32,
    is_sticky: bool,
    is_locked: bool,
    is_deleted: bool,
    category_id: i16,
    min_level: i32,
    created_at: String,
    updated_at: String,
}

impl From<Topic> for DanbooruTopic {
    fn from(t: Topic) -> Self {
        Self {
            id: t.id,
            creator_id: t.creator_id,
            updater_id: t.creator_id,
            title: t.title,
            response_count: (t.post_count - 1).max(0),
            is_sticky: t.is_sticky,
            is_locked: t.is_locked,
            is_deleted: t.is_deleted,
            category_id: t.category_id,
            min_level: 0,
            created_at: timestamp(t.created_at),
            updated_at: timestamp(t.last_posted_at),
        }
    }
}

#[derive(Debug, Serialize)]
struct DanbooruForumPost {
    id: i64,
    topic_id: i64,
    creator_id: Option<i64>,
    updater_id: Option<i64>,
    body: String,
    is_deleted: bool,
    created_at: String,
    updated_at: String,
}

impl From<Post> for DanbooruForumPost {
    fn from(p: Post) -> Self {
        Self {
            id: p.id,
            topic_id: p.topic_id,
            creator_id: p.creator_id,
            updater_id: p.creator_id,
            body: p.body,
            is_deleted: p.is_hidden,
            created_at: timestamp(p.created_at),
            updated_at: timestamp(p.updated_at),
        }
    }
}

fn number<T: std::str::FromStr>(value: &str) -> Option<Option<T>> {
    match value.trim() {
        "" => Some(None),
        v => v.parse().ok().map(Some),
    }
}

#[derive(Debug, Default, Deserialize)]
struct TopicParams {
    #[serde(rename = "search[title_matches]", default)]
    title_matches: String,
    #[serde(rename = "search[title]", default)]
    title: String,
    #[serde(rename = "search[category_id]", default)]
    category_id: String,
    #[serde(rename = "search[id]", default)]
    id: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn topics(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<TopicParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let (offset, limit) = window(&params.list, 1000)?;
    let title = if params.title_matches.is_empty() {
        &params.title
    } else {
        &params.title_matches
    };
    let title = title.replace('*', " ");
    let ids: Option<Vec<i64>> = match params.id.trim() {
        "" => None,
        list => Some(
            list.split(',')
                .filter_map(|i| i.trim().parse().ok())
                .collect(),
        ),
    };
    let filter = TopicFilter {
        category_id: number(&params.category_id).flatten(),
        title: title.trim(),
        with_deleted: moderates(&current),
        creator_id: None,
        ids: ids.as_deref(),
    };
    let list: Vec<DanbooruTopic> =
        forum::topics(state.reader(&current), None, &filter, offset, limit)
            .await?
            .into_iter()
            .map(DanbooruTopic::from)
            .collect();
    json(list, &params.list.only)
}

async fn topic(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let topic = crate::forum::visible_topic(&state, &current, id).await?;
    json(DanbooruTopic::from(topic), "")
}

#[derive(Debug, Default, Deserialize)]
struct PostParams {
    #[serde(rename = "search[topic_id]", default)]
    topic_id: String,
    #[serde(rename = "search[creator_id]", default)]
    creator_id: String,
    #[serde(rename = "search[creator_name]", default)]
    creator_name: String,
    #[serde(rename = "search[body_matches]", default)]
    body_matches: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn posts(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<PostParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let (offset, limit) = window(&params.list, 1000)?;
    let creator_id = match (params.creator_id.trim(), params.creator_name.trim()) {
        ("", "") => None,
        ("", name) => Some(
            moekura_db::users::by_name(db, name)
                .await?
                .map_or(-1, |u| u.id),
        ),
        (id, _) => Some(id.parse().unwrap_or(-1)),
    };
    let staff = moderates(&current);
    // Newest first, as Danbooru lists them, even for one topic.
    let filter = PostFilter {
        topic_id: number(&params.topic_id).unwrap_or(Some(-1)),
        creator_id,
        words: &params.body_matches.replace('*', " "),
        with_hidden: staff,
        with_deleted_topics: staff,
        newest_first: true,
    };
    let list: Vec<DanbooruForumPost> = forum::posts(db, &filter, offset, limit)
        .await?
        .into_iter()
        .map(DanbooruForumPost::from)
        .collect();
    json(list, &params.list.only)
}

async fn post(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let p = forum::post(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    crate::forum::visible_topic(&state, &current, p.topic_id).await?;
    if p.is_hidden && !moderates(&current) {
        return Err(AppError::NotFound);
    }
    json(DanbooruForumPost::from(p), "")
}

async fn create_post(
    State(state): State<AppState>,
    current: CurrentUser,
    fields: Fields,
) -> Result<Response, AppError> {
    let topic: i64 = fields
        .get("forum_post[topic_id]")
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| AppError::Unprocessable("`forum_post[topic_id]` is required".into()))?;
    let body = fields.get("forum_post[body]").unwrap_or_default();
    let id = crate::forum::add_post(&state, &current, topic, body)
        .await?
        .id;
    let p = forum::post(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, axum::Json(DanbooruForumPost::from(p))).into_response())
}

async fn create_topic(
    State(state): State<AppState>,
    current: CurrentUser,
    fields: Fields,
) -> Result<Response, AppError> {
    let category: i16 = match fields.get("forum_topic[category_id]") {
        Some(c) => c
            .trim()
            .parse()
            .map_err(|_| AppError::Unprocessable("Choose a category.".into()))?,
        None => forum::categories(state.db.primary())
            .await?
            .iter()
            .find(|c| crate::forum::may_post_in(&current, c))
            .map(|c| c.id)
            .ok_or(AppError::NotFound)?,
    };
    let title = fields.get("forum_topic[title]").unwrap_or_default();
    let body = fields
        .get("forum_topic[original_post_attributes][body]")
        .unwrap_or_default();
    let id = crate::forum::start_topic(&state, &current, category, title, body)
        .await?
        .id;
    // Held topics are deleted, so not visible to their creator.
    let viewer = current.user.as_ref().map(|u| u.id);
    let topic = forum::topic(state.db.primary(), viewer, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, axum::Json(DanbooruTopic::from(topic))).into_response())
}

#[derive(Debug, Serialize)]
struct DanbooruVote {
    id: i64,
    forum_post_id: i64,
    creator_id: i64,
    score: i16,
    created_at: String,
    updated_at: String,
}

async fn votes(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<ListParams>,
) -> Result<Response, AppError> {
    let me = current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)?;
    let (offset, limit) = window(&params, 1000)?;
    let list: Vec<DanbooruVote> = forum::votes(state.db.primary(), me, offset, limit)
        .await?
        .into_iter()
        .map(|v| {
            let at = timestamp(v.created_at);
            DanbooruVote {
                // Votes have no id of their own: the post's.
                id: v.post_id,
                forum_post_id: v.post_id,
                creator_id: v.user_id,
                score: v.score,
                updated_at: at.clone(),
                created_at: at,
            }
        })
        .collect();
    json(list, &params.only)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::app;
    use crate::test_support::session_for;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn forum(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let body = json!({ "forum_topic": {
            "title": "Hello", "original_post_attributes": { "body": "First" }
        }});
        let created = app
            .json("POST", "/forum_topics.json", Some(&alice), Some(body))
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let parse = |body: &str| serde_json::from_str::<Value>(body).unwrap();
        let topic = parse(&created.body)["id"].as_i64().unwrap();
        let reply = json!({ "forum_post": { "topic_id": topic, "body": "Second" } });
        let replied = app
            .json("POST", "/forum_posts.json", Some(&alice), Some(reply))
            .await;
        assert_eq!(replied.status, StatusCode::CREATED, "{}", replied.body);

        let topics = parse(
            &app.get("/forum_topics.json?search[title_matches]=hel*", None)
                .await
                .body,
        );
        assert_eq!(topics[0]["response_count"], 1);
        let posts = parse(
            &app.get(&format!("/forum_posts.json?search[topic_id]={topic}"), None)
                .await
                .body,
        );
        assert_eq!(posts[0]["body"], "Second", "newest first");
        assert_eq!(posts.as_array().unwrap().len(), 2);
        let one = parse(
            &app.get(&format!("/forum_topics/{topic}.json"), None)
                .await
                .body,
        );
        assert_eq!(one["title"], "Hello");
        assert_eq!(
            parse(&app.get("/forum_post_votes.json", Some(&alice)).await.body),
            json!([])
        );
    }
}
