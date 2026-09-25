//! Comments, comment votes, pools, favorite groups and saved searches in
//! Danbooru's shapes: `/comments.json`, `/comments/{id}.json`,
//! `/comments/{id}/votes.json`, `/comment_votes.json`, `/pools.json`,
//! `/pools/{id}.json`, `/favorite_groups.json`,
//! `/favorite_groups/{id}.json`, `/saved_searches.json` and
//! `/saved_searches/{id}.json`.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use moekura_core::permissions::Permission;
use moekura_db::comments::{self, Comment, Filter};
use moekura_db::favorite_groups::{self, Group};
use moekura_db::pools::{self, Pool};
use moekura_db::{saved_searches, users};
use serde::{Deserialize, Serialize};

use super::{Fields, ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::posts::visibility;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/comments", get(list_comments).post(create_comment))
        .route(
            "/comments/{id}",
            get(show_comment)
                .put(update_comment)
                .patch(update_comment)
                .delete(delete_comment),
        )
        .route(
            "/comments/{id}/votes",
            post(vote_comment).delete(unvote_comment),
        )
        .route("/comment_votes", get(comment_votes))
        .route("/pools", get(list_pools))
        .route("/pools/{id}", get(show_pool))
        .route("/favorite_groups", get(list_groups))
        .route("/favorite_groups/{id}", get(show_group))
        .route("/saved_searches", get(list_saved).post(create_saved))
        .route("/saved_searches/{id}", axum::routing::delete(delete_saved))
}

/// `{"name": …}`-style ids from a path, which may not be numbers.
fn number<T: std::str::FromStr>(raw: &str) -> Result<T, AppError> {
    raw.parse().map_err(|_| AppError::NotFound)
}

/// A page number from `page`, 1-based.
fn page_number(list: &ListParams) -> i64 {
    list.page.trim().parse().unwrap_or(1).clamp(1, 1000)
}

#[derive(Debug, Serialize)]
struct Creator {
    id: i64,
    name: String,
}

#[derive(Debug, Serialize)]
struct DanbooruComment {
    id: i64,
    post_id: i64,
    creator_id: Option<i64>,
    updater_id: Option<i64>,
    creator: Option<Creator>,
    body: String,
    score: i32,
    created_at: String,
    updated_at: String,
    is_deleted: bool,
    is_sticky: bool,
    do_not_bump_post: bool,
}

impl From<Comment> for DanbooruComment {
    fn from(c: Comment) -> Self {
        let created = timestamp(c.created_at);
        Self {
            id: c.id,
            post_id: c.post_id,
            creator_id: c.creator_id,
            updater_id: c.creator_id,
            creator: c
                .creator_id
                .zip(c.creator_name)
                .map(|(id, name)| Creator { id, name }),
            body: c.body,
            score: c.score,
            updated_at: c.edited_at.map_or_else(|| created.clone(), timestamp),
            created_at: created,
            is_deleted: c.is_deleted,
            is_sticky: false,
            do_not_bump_post: false,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct CommentParams {
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(rename = "search[creator_id]", default)]
    creator_id: String,
    #[serde(rename = "search[creator_name]", default)]
    creator_name: String,
    #[serde(flatten)]
    list: ListParams,
}

/// Comments, newest first, on posts the requester can see. Only numbered
/// pages of `b<id>` for comments before one.
async fn list_comments(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<CommentParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let creator_id = match (params.creator_id.trim(), params.creator_name.trim()) {
        (id, _) if !id.is_empty() => Some(id.parse().unwrap_or(-1)),
        (_, name) if !name.is_empty() => Some(users::by_name(db, name).await?.map_or(-1, |u| u.id)),
        _ => None,
    };
    let filter = Filter {
        post_id: params.post_id.trim().parse().ok(),
        creator_id,
        with_deleted: crate::comments::sees_deleted(&current),
    };
    let limit = i64::from(params.list.limit(1000));
    // `b<id>` pages go by id; numbered ones skip ahead.
    let (before, skip) = match params.list.page.trim().strip_prefix('b') {
        Some(id) => (id.parse().ok(), 0),
        None => (None, (page_number(&params.list) - 1) * limit),
    };
    let mut found =
        comments::list(db, &visibility(&current), &filter, before, skip + limit).await?;
    let found: Vec<DanbooruComment> = found
        .drain(..)
        .skip(skip as usize)
        .map(DanbooruComment::from)
        .collect();
    json(found, &params.list.only)
}

async fn show_comment(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(list): Query<ListParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let (comment, _) = crate::comments::visible_comment(&state, &current, number(&id)?).await?;
    json(DanbooruComment::from(comment), &list.only)
}

async fn create_comment(
    State(state): State<AppState>,
    current: CurrentUser,
    fields: Fields,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let post_id: i64 = fields
        .get("comment[post_id]")
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| AppError::Unprocessable("`comment[post_id]` is required".into()))?;
    let db = state.db.primary();
    let post = moekura_db::posts::by_id(db, post_id)
        .await?
        .filter(|p| visibility(&current).allows(p))
        .ok_or(AppError::NotFound)?;
    let body = crate::comments::clean_body(fields.get("comment[body]").unwrap_or_default())?;
    let user = crate::comments::commenter(&state, &current, &post).await?;
    let id = comments::create(db, post_id, user, &body).await?;
    let comment = comments::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    Ok((
        StatusCode::CREATED,
        axum::Json(DanbooruComment::from(comment)),
    )
        .into_response())
}

async fn update_comment(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    fields: Fields,
) -> Result<Response, AppError> {
    let id = number(&id)?;
    let (comment, _) = crate::comments::visible_comment(&state, &current, id).await?;
    crate::comments::check_author(&current, &comment)?;
    let body = crate::comments::clean_body(fields.get("comment[body]").unwrap_or_default())?;
    let db = state.db.primary();
    comments::update(db, id, &body).await?;
    let comment = comments::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    json(DanbooruComment::from(comment), "")
}

async fn delete_comment(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id = number(&id)?;
    let (comment, _) = crate::comments::visible_comment(&state, &current, id).await?;
    crate::comments::check_author(&current, &comment)?;
    comments::set_deleted(state.db.primary(), id, true).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Debug, Serialize)]
struct CommentVote {
    /// Votes have no ids of their own here: the comment's.
    id: i64,
    comment_id: i64,
    user_id: i64,
    score: i16,
    is_deleted: bool,
}

async fn vote_comment(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    fields: Fields,
) -> Result<Response, AppError> {
    let id = number(&id)?;
    let score: i16 = fields
        .get("score")
        .and_then(|v| v.trim().parse().ok())
        .filter(|s| matches!(s, 1 | -1))
        .ok_or_else(|| AppError::Unprocessable("`score` is 1 or -1".into()))?;
    crate::comments::vote_on(&state, &current, id, score).await?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let vote = CommentVote {
        id,
        comment_id: id,
        user_id: user.id,
        score,
        is_deleted: false,
    };
    Ok((StatusCode::CREATED, axum::Json(vote)).into_response())
}

async fn unvote_comment(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    crate::comments::vote_on(&state, &current, number(&id)?, 0).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Debug, Default, Deserialize)]
struct CommentVoteParams {
    #[serde(rename = "search[comment_id]", default)]
    comment_id: String,
    #[serde(flatten)]
    list: ListParams,
}

/// The requester's comment votes, as with `/post_votes.json`.
async fn comment_votes(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<CommentVoteParams>,
) -> Result<Response, AppError> {
    let Some(user) = &current.user else {
        return json(Vec::<CommentVote>::new(), "");
    };
    let comment_ids: Vec<i64> = params
        .comment_id
        .split([' ', ','])
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let limit = i64::from(params.list.limit(1000));
    let rows: Vec<(i64, i16)> = sqlx::query_as(
        "SELECT comment_id, score FROM comment_votes
         WHERE user_id = $1 AND (cardinality($2::bigint[]) = 0 OR comment_id = ANY($2))
         ORDER BY created_at DESC OFFSET $3 LIMIT $4",
    )
    .bind(user.id)
    .bind(&comment_ids)
    .bind((page_number(&params.list) - 1) * limit)
    .bind(limit)
    .fetch_all(state.db.primary())
    .await?;
    let votes: Vec<CommentVote> = rows
        .into_iter()
        .map(|(comment_id, score)| CommentVote {
            id: comment_id,
            comment_id,
            user_id: user.id,
            score,
            is_deleted: false,
        })
        .collect();
    json(votes, &params.list.only)
}

#[derive(Debug, Serialize)]
struct DanbooruPool {
    id: i32,
    name: String,
    description: String,
    category: String,
    is_active: bool,
    is_deleted: bool,
    post_ids: Vec<i64>,
    post_count: usize,
    created_at: String,
    updated_at: String,
}

async fn pool_json(
    state: &AppState,
    current: &CurrentUser,
    pool: Pool,
) -> Result<DanbooruPool, AppError> {
    let post_ids = pools::visible_post_ids(
        state.db.primary(),
        pool.id,
        &visibility(current),
        0,
        i64::MAX,
    )
    .await?;
    Ok(DanbooruPool {
        id: pool.id,
        name: pool.name,
        description: pool.description,
        category: pool.category,
        is_active: !pool.is_deleted,
        is_deleted: pool.is_deleted,
        post_count: post_ids.len(),
        post_ids,
        created_at: timestamp(pool.created_at),
        updated_at: timestamp(pool.updated_at),
    })
}

#[derive(Debug, Default, Deserialize)]
struct PoolParams {
    #[serde(rename = "search[name_matches]", default)]
    name_matches: String,
    #[serde(rename = "search[name_contains]", default)]
    name_contains: String,
    #[serde(rename = "search[id]", default)]
    id: String,
    #[serde(rename = "search[category]", default)]
    category: String,
    #[serde(flatten)]
    list: ListParams,
}

/// Pools, most recently changed first.
async fn list_pools(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<PoolParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let ids: Vec<i32> = params
        .id
        .split([' ', ','])
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let limit = i64::from(params.list.limit(1000));
    let offset = (page_number(&params.list) - 1) * limit;
    let found: Vec<Pool> = if ids.is_empty() {
        let contains = params.name_contains.trim();
        let name = if contains.is_empty() {
            params.name_matches.trim().to_owned()
        } else {
            format!("*{contains}*")
        };
        let filter = pools::Filter {
            name: &name.split_whitespace().collect::<Vec<_>>().join("_"),
            category: moekura_core::pools::Category::parse(params.category.trim())
                .map(|c| c.as_str()),
            with_deleted: false,
        };
        pools::list(db, &filter, offset, limit).await?
    } else {
        let mut found = Vec::new();
        for id in ids.into_iter().skip(offset as usize).take(limit as usize) {
            if let Some(pool) = pools::by_id(db, id).await?.filter(|p| !p.is_deleted) {
                found.push(pool);
            }
        }
        found
    };
    let mut out = Vec::with_capacity(found.len());
    for pool in found {
        out.push(pool_json(&state, &current, pool).await?);
    }
    json(out, &params.list.only)
}

async fn show_pool(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(list): Query<ListParams>,
) -> Result<Response, AppError> {
    let pool = crate::pools::visible_pool(state.reader(&current), &current, number(&id)?).await?;
    json(pool_json(&state, &current, pool).await?, &list.only)
}

#[derive(Debug, Serialize)]
struct DanbooruGroup {
    id: i32,
    name: String,
    creator_id: i64,
    creator: Creator,
    post_ids: Vec<i64>,
    is_public: bool,
    created_at: String,
    updated_at: String,
}

async fn group_json(
    state: &AppState,
    current: &CurrentUser,
    group: Group,
) -> Result<DanbooruGroup, AppError> {
    let post_ids = favorite_groups::visible_post_ids(
        state.db.primary(),
        group.id,
        &visibility(current),
        0,
        i64::MAX,
    )
    .await?;
    Ok(DanbooruGroup {
        id: group.id,
        name: group.name,
        creator_id: group.creator_id,
        creator: Creator {
            id: group.creator_id,
            name: group.creator_name,
        },
        post_ids,
        is_public: group.is_public,
        created_at: timestamp(group.created_at),
        updated_at: timestamp(group.updated_at),
    })
}

#[derive(Debug, Default, Deserialize)]
struct GroupParams {
    #[serde(rename = "search[creator_id]", default)]
    creator_id: String,
    #[serde(rename = "search[creator_name]", default)]
    creator_name: String,
    #[serde(flatten)]
    list: ListParams,
}

/// A user's favorite groups: public ones, and all of the requester's.
/// Without a creator, the requester's.
async fn list_groups(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<GroupParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let me = current.user.as_ref().map(|u| u.id);
    let creator = match (params.creator_id.trim(), params.creator_name.trim()) {
        (id, _) if !id.is_empty() => id.parse().ok(),
        (_, name) if !name.is_empty() => users::by_name(db, name).await?.map(|u| u.id),
        _ => me,
    };
    let Some(creator) = creator else {
        return json(Vec::<DanbooruGroup>::new(), "");
    };
    let limit = params.list.limit(1000) as usize;
    let skip = (page_number(&params.list) as usize - 1) * limit;
    let groups = favorite_groups::for_user(db, creator, me == Some(creator)).await?;
    let mut out = Vec::new();
    for group in groups.into_iter().skip(skip).take(limit) {
        out.push(group_json(&state, &current, group).await?);
    }
    json(out, &params.list.only)
}

async fn show_group(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(list): Query<ListParams>,
) -> Result<Response, AppError> {
    let group =
        crate::favorite_groups::visible_group(state.reader(&current), &current, number(&id)?)
            .await?;
    json(group_json(&state, &current, group).await?, &list.only)
}

#[derive(Debug, Serialize)]
struct DanbooruSavedSearch {
    id: i64,
    user_id: i64,
    query: String,
    labels: Vec<String>,
    created_at: String,
    updated_at: String,
}

fn user_id(current: &CurrentUser) -> Result<i64, AppError> {
    current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)
}

async fn list_saved(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(list): Query<ListParams>,
) -> Result<Response, AppError> {
    let user = user_id(&current)?;
    let found: Vec<DanbooruSavedSearch> = saved_searches::for_user(state.db.primary(), user)
        .await?
        .into_iter()
        .map(|s| DanbooruSavedSearch {
            id: s.id,
            user_id: user,
            query: s.query,
            labels: s.labels,
            created_at: timestamp(s.created_at),
            updated_at: timestamp(s.created_at),
        })
        .collect();
    json(found, &list.only)
}

async fn create_saved(
    State(state): State<AppState>,
    current: CurrentUser,
    fields: Fields,
) -> Result<Response, AppError> {
    let user = user_id(&current)?;
    let query = fields.get("saved_search[query]").unwrap_or_default();
    let labels = fields.get("saved_search[label_string]").unwrap_or_default();
    let id = crate::saved_searches::save(&state, &current, query, labels).await?;
    let saved = saved_searches::for_user(state.db.primary(), user)
        .await?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or(AppError::NotFound)?;
    let body = DanbooruSavedSearch {
        id,
        user_id: user,
        query: saved.query,
        labels: saved.labels,
        created_at: timestamp(saved.created_at),
        updated_at: timestamp(saved.created_at),
    };
    Ok((StatusCode::CREATED, axum::Json(body)).into_response())
}

async fn delete_saved(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let user = user_id(&current)?;
    if !saved_searches::remove(state.db.primary(), user, number(&id)?).await? {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    fn parse(body: &str) -> Value {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn comments_and_votes(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let post = upload(&app, &alice, 20, "cat").await;

        let created = app
            .post_form(
                "/comments.json",
                Some(&alice),
                &[],
                &format!("comment[post_id]={post}&comment[body]=Nice"),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let id = parse(&created.body)["id"].as_i64().unwrap();

        let listed = parse(
            &app.get(
                &format!("/comments.json?search[post_id]={post}&group_by=comment"),
                None,
            )
            .await
            .body,
        );
        assert_eq!(listed[0]["body"], json!("Nice"));
        assert_eq!(listed[0]["creator"]["name"], json!("alice"));
        let voted = app
            .post_form(
                &format!("/comments/{id}/votes.json"),
                Some(&bob),
                &[],
                "score=1",
            )
            .await;
        assert_eq!(voted.status, StatusCode::CREATED, "{}", voted.body);
        let votes = parse(&app.get("/comment_votes.json", Some(&bob)).await.body);
        assert_eq!(votes[0]["comment_id"], json!(id));
        assert_eq!(
            parse(
                &app.get(&format!("/comments/{id}.json?only=score"), None)
                    .await
                    .body
            ),
            json!({ "score": 1 })
        );
        let post_json = parse(&app.get(&format!("/posts/{post}.json"), None).await.body);
        assert!(post_json["last_commented_at"].is_string());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn pools_groups_and_saved_searches(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let a = upload(&app, &alice, 20, "cat").await;
        let b = upload(&app, &alice, 24, "dog").await;
        let id = moekura_db::pools::create(
            &pool,
            &moekura_db::pools::Contents {
                name: "My_Comic".into(),
                description: "Pages.".into(),
                category: "series".into(),
                is_deleted: false,
                post_ids: vec![b, a],
            },
            None,
        )
        .await
        .unwrap();
        let listed = parse(
            &app.get("/pools.json?search[name_matches]=my*", None)
                .await
                .body,
        );
        assert_eq!(listed[0]["post_ids"], json!([b, a]));
        assert_eq!(listed[0]["category"], json!("series"));
        assert_eq!(
            parse(&app.get(&format!("/pools/{id}.json"), None).await.body)["name"],
            json!("My_Comic")
        );

        let alice_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'alice'")
            .fetch_one(&pool)
            .await
            .unwrap();
        moekura_db::favorite_groups::create(
            &pool,
            alice_id,
            &moekura_db::favorite_groups::Contents {
                name: "Best".into(),
                is_public: true,
                post_ids: vec![a],
            },
        )
        .await
        .unwrap();
        let groups = parse(
            &app.get("/favorite_groups.json?search[creator_name]=alice", None)
                .await
                .body,
        );
        assert_eq!(
            (&groups[0]["name"], &groups[0]["post_ids"]),
            (&json!("Best"), &json!([a]))
        );

        let saved = app
            .post_form(
                "/saved_searches.json",
                Some(&alice),
                &[],
                "saved_search[query]=cat&saved_search[label_string]=pets",
            )
            .await;
        assert_eq!(saved.status, StatusCode::CREATED, "{}", saved.body);
        let listed = parse(&app.get("/saved_searches.json", Some(&alice)).await.body);
        assert_eq!(listed[0]["labels"], json!(["pets"]));
        assert_eq!(
            app.get("/saved_searches.json", None).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
}
