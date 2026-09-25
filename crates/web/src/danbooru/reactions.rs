//! Favorites and votes: `/favorites.json`, `/favorites/{post_id}.json`,
//! `/posts/{id}/favorites.json`, `/posts/{id}/votes.json`,
//! `/post_votes.json` and `/post_votes/{id}.json`.
//!
//! Moekura keeps one favorite and one vote per user and post without ids
//! of their own, so both are identified by their post's id.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use moekura_core::permissions::Permission;
use moekura_db::favorites as db_favorites;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;

use super::{Fields, ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::favorites::{user_for, vote_on};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/favorites", get(favorites).post(add_favorite))
        .route(
            "/favorites/{post_id}",
            axum::routing::delete(remove_favorite),
        )
        .route("/posts/{id}/favorites", get(favorited_by))
        .route("/posts/{id}/votes", post(vote).delete(unvote))
        .route("/post_votes", get(votes))
        .route("/post_votes/{id}", axum::routing::delete(unvote))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Favorite {
    id: i64,
    user_id: i64,
    post_id: i64,
}

fn post_id(value: Option<&str>) -> Result<i64, AppError> {
    value
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| AppError::Unprocessable("`post_id` is required".into()))
}

async fn add_favorite(
    State(state): State<AppState>,
    current: CurrentUser,
    fields: Fields,
) -> Result<Response, AppError> {
    let id = post_id(fields.get("post_id"))?;
    let user = user_for(&state, &current, id, Permission::Favorite).await?;
    db_favorites::add(state.db.primary(), user, id).await?;
    let favorite = Favorite {
        id,
        user_id: user,
        post_id: id,
    };
    Ok((StatusCode::CREATED, axum::Json(favorite)).into_response())
}

async fn remove_favorite(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let user = user_for(&state, &current, id, Permission::Favorite).await?;
    db_favorites::remove(state.db.primary(), user, id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Numbers from a list separated by spaces or commas.
fn ids(value: &str) -> Vec<i64> {
    value
        .split([' ', ','])
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

#[derive(Debug, Default, Deserialize)]
struct FavoriteParams {
    #[serde(rename = "search[user_id]", default)]
    user_id: String,
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(flatten)]
    list: ListParams,
}

/// Favorites, newest first; by default the requester's.
async fn favorites(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<FavoriteParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let user_id = match params.user_id.trim().parse::<i64>() {
        Ok(id) => Some(id),
        Err(_) => current.user.as_ref().map(|u| u.id),
    };
    let post_ids = ids(&params.post_id);
    let limit = i64::from(params.list.limit(1000));
    let page: i64 = params.list.page.trim().parse().unwrap_or(1).clamp(1, 1000);
    let found: Vec<Favorite> = sqlx::query_as(
        "SELECT f.post_id AS id, f.user_id, f.post_id FROM favorites f
         JOIN posts p ON p.id = f.post_id AND p.status IN ('active', 'flagged')
         WHERE ($1::bigint IS NULL OR f.user_id = $1)
           AND (cardinality($2::bigint[]) = 0 OR f.post_id = ANY($2))
         ORDER BY f.created_at DESC OFFSET $3 LIMIT $4",
    )
    .bind(user_id)
    .bind(&post_ids)
    .bind((page - 1) * limit)
    .bind(limit)
    .fetch_all(state.reader(&current))
    .await?;
    json(found, &params.list.only)
}

/// Who favorited a post, as users.
async fn favorited_by(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Query(params): Query<ListParams>,
) -> Result<Response, AppError> {
    // The post itself must be visible.
    crate::api::posts::one(&state, &current, id).await?;
    let db = state.reader(&current);
    let limit = i64::from(params.limit(100));
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT f.user_id FROM favorites f JOIN users u ON u.id = f.user_id AND u.status = 'active'
         WHERE f.post_id = $1 ORDER BY f.created_at DESC LIMIT $2",
    )
    .bind(id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    let mut users = Vec::new();
    for user in moekura_db::users::by_ids(db, &ids).await? {
        users.push(super::users::danbooru_user(&state, db, &user).await?);
    }
    json(users, &params.only)
}

#[derive(Debug, Serialize)]
struct PostVote {
    id: i64,
    post_id: i64,
    user_id: i64,
    score: i16,
    is_deleted: bool,
    created_at: String,
    updated_at: String,
}

async fn vote_of(db: &PgPool, user: i64, post: i64) -> Result<Option<PostVote>, AppError> {
    let row: Option<(i16, OffsetDateTime)> = sqlx::query_as(
        "SELECT score, created_at FROM post_votes WHERE user_id = $1 AND post_id = $2",
    )
    .bind(user)
    .bind(post)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(score, at)| PostVote {
        id: post,
        post_id: post,
        user_id: user,
        score,
        is_deleted: false,
        created_at: timestamp(at),
        updated_at: timestamp(at),
    }))
}

/// Votes a post up (`score=1`) or down (`score=-1`); Danbooru's older
/// `up` and `down` work too.
async fn vote(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    fields: Fields,
) -> Result<Response, AppError> {
    let score: i16 = match fields.get("score").map(str::trim) {
        Some("1" | "up") => 1,
        Some("-1" | "down") => -1,
        _ => return Err(AppError::Unprocessable("`score` must be 1 or -1".into())),
    };
    let user = user_for(&state, &current, id, Permission::Vote).await?;
    let db = state.db.primary();
    vote_on(db, id, user, score).await?;
    let vote = vote_of(db, user, id).await?.ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, axum::Json(vote)).into_response())
}

/// Takes back the requester's vote on a post (also at
/// `/post_votes/{id}`, whose id is the post's).
async fn unvote(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let user = user_for(&state, &current, id, Permission::Vote).await?;
    vote_on(state.db.primary(), id, user, 0).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Debug, Default, Deserialize)]
struct VoteParams {
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(rename = "search[user_id]", default)]
    user_id: String,
    #[serde(flatten)]
    list: ListParams,
}

/// The requester's votes. Who voted how is private, so others' aren't
/// listed.
async fn votes(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<VoteParams>,
) -> Result<Response, AppError> {
    let Some(me) = current.user.as_ref().map(|u| u.id) else {
        return json(Vec::<PostVote>::new(), "");
    };
    if params
        .user_id
        .trim()
        .parse::<i64>()
        .is_ok_and(|id| id != me)
    {
        return json(Vec::<PostVote>::new(), "");
    }
    let post_ids = ids(&params.post_id);
    let limit = i64::from(params.list.limit(1000));
    let rows: Vec<(i64, i16, OffsetDateTime)> = sqlx::query_as(
        "SELECT post_id, score, created_at FROM post_votes
         WHERE user_id = $1 AND (cardinality($2::bigint[]) = 0 OR post_id = ANY($2))
         ORDER BY created_at DESC LIMIT $3",
    )
    .bind(me)
    .bind(&post_ids)
    .bind(limit)
    .fetch_all(state.reader(&current))
    .await?;
    let votes: Vec<PostVote> = rows
        .into_iter()
        .map(|(post, score, at)| PostVote {
            id: post,
            post_id: post,
            user_id: me,
            score,
            is_deleted: false,
            created_at: timestamp(at),
            updated_at: timestamp(at),
        })
        .collect();
    json(votes, &params.list.only)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    fn body(response: &crate::test_support::TestResponse) -> Value {
        serde_json::from_str(&response.body).unwrap_or_else(|_| panic!("{}", response.body))
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn favorites_and_votes(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let id = upload(&app, &alice, 20, "cat").await;

        let anonymous = app
            .json("POST", &format!("/favorites.json?post_id={id}"), None, None)
            .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
        let added = app
            .json(
                "POST",
                &format!("/favorites.json?post_id={id}"),
                Some(&bob),
                None,
            )
            .await;
        assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
        assert_eq!(body(&added)["post_id"], json!(id));
        // A form body works too.
        let form = app
            .post_form(
                "/favorites.json",
                Some(&alice),
                &[],
                &format!("post_id={id}"),
            )
            .await;
        assert_eq!(form.status, StatusCode::CREATED, "{}", form.body);

        let mine = body(&app.get("/favorites.json", Some(&bob)).await);
        assert_eq!(mine.as_array().unwrap().len(), 1);
        let post = body(&app.get(&format!("/posts/{id}.json"), None).await);
        assert_eq!(post["fav_count"], json!(2));
        let fans = body(&app.get(&format!("/posts/{id}/favorites.json"), None).await);
        assert_eq!(fans.as_array().unwrap().len(), 2);
        let removed = app
            .json("DELETE", &format!("/favorites/{id}.json"), Some(&bob), None)
            .await;
        assert_eq!(removed.status, StatusCode::NO_CONTENT);
        assert_eq!(
            body(&app.get("/favorites.json", Some(&bob)).await),
            json!([])
        );

        let up = app
            .json(
                "POST",
                &format!("/posts/{id}/votes.json?score=1"),
                Some(&bob),
                None,
            )
            .await;
        assert_eq!(up.status, StatusCode::CREATED, "{}", up.body);
        assert_eq!(body(&up)["score"], json!(1));
        let bad = app
            .json(
                "POST",
                &format!("/posts/{id}/votes.json?score=5"),
                Some(&bob),
                None,
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        let post = body(&app.get(&format!("/posts/{id}.json"), None).await);
        assert_eq!(
            (post["score"].clone(), post["up_score"].clone()),
            (json!(1), json!(1))
        );
        let votes = body(
            &app.get(
                &format!("/post_votes.json?search[post_id]={id}"),
                Some(&bob),
            )
            .await,
        );
        assert_eq!(votes[0]["score"], json!(1));
        assert_eq!(
            body(&app.get("/post_votes.json", Some(&alice)).await),
            json!([])
        );
        let taken_back = app
            .json(
                "DELETE",
                &format!("/post_votes/{id}.json"),
                Some(&bob),
                None,
            )
            .await;
        assert_eq!(taken_back.status, StatusCode::NO_CONTENT);
        let post = body(&app.get(&format!("/posts/{id}.json"), None).await);
        assert_eq!(post["score"], json!(0));
    }
}
