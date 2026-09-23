//! Favoriting and voting on posts. Plain forms that redirect back to the
//! post; the page script sends them with `Accept: application/json` and
//! gets the new counts instead, to skip the reload.

use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::http::header::ACCEPT;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use uwuu_core::permissions::Permission;
use uwuu_db::{favorites, posts};

use crate::AppState;
use crate::error::AppError;
use crate::pages::Page;
use crate::posts::visibility;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/posts/{id}/favorite", post(favorite))
        .route("/posts/{id}/vote", post(vote))
}

#[derive(Debug, Default, Deserialize)]
struct BackQuery {
    /// The search the post was opened from.
    #[serde(default)]
    q: String,
}

#[derive(Debug, Deserialize)]
struct FavoriteForm {
    /// `add` or `remove`.
    action: String,
}

#[derive(Debug, Deserialize)]
struct VoteForm {
    /// `1`, `-1`, or `0` to take the vote back.
    score: i16,
}

/// The post's counts and the viewer's part in them, for the script.
#[derive(Debug, Serialize)]
struct Reactions {
    fav_count: i32,
    favorited: bool,
    score: i32,
    vote: i16,
}

/// Checks the user may act on post `id` and returns their id.
async fn user_for(page: &Page, id: i64, permission: Permission) -> Result<i64, AppError> {
    page.current.require(permission)?;
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let post = posts::by_id(page.state().db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !visibility(&page.current).allows(&post) {
        return Err(AppError::NotFound);
    }
    Ok(user.id)
}

async fn respond(
    page: &Page,
    headers: &HeaderMap,
    id: i64,
    user: i64,
    back: &BackQuery,
) -> Result<Response, AppError> {
    let wants_json = headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"));
    if !wants_json {
        let mut url = format!("/posts/{id}");
        if !back.q.is_empty() {
            let q = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("q", &back.q)
                .finish();
            url = format!("{url}?{q}");
        }
        return Ok(Redirect::to(&url).into_response());
    }
    let db = page.state().db.primary();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    Ok(Json(Reactions {
        fav_count: post.fav_count,
        favorited: favorites::exists(db, user, id).await?,
        score: post.score,
        vote: favorites::vote_of(db, user, id).await?,
    })
    .into_response())
}

async fn favorite(
    page: Page,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(back): Query<BackQuery>,
    Form(form): Form<FavoriteForm>,
) -> Result<Response, AppError> {
    let user = user_for(&page, id, Permission::Favorite).await?;
    let db = page.state().db.primary();
    match form.action.as_str() {
        "add" => favorites::add(db, user, id).await?,
        "remove" => favorites::remove(db, user, id).await?,
        _ => return Err(AppError::BadRequest("Unknown action".into())),
    };
    respond(&page, &headers, id, user, &back).await
}

async fn vote(
    page: Page,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(back): Query<BackQuery>,
    Form(form): Form<VoteForm>,
) -> Result<Response, AppError> {
    let user = user_for(&page, id, Permission::Vote).await?;
    if !(-1..=1).contains(&form.score) {
        return Err(AppError::BadRequest("A vote is 1, -1 or 0".into()));
    }
    favorites::vote(page.state().db.primary(), user, id, form.score).await?;
    respond(&page, &headers, id, user, &back).await
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use sqlx::PgPool;
    use uwuu_core::permissions::SystemRole;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn favorites_and_votes(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::posts::routes()),
        );
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
             VALUES ($1, $2, $3, 'png', 10, 10, 1, 'original/aa/aa/x.png')",
        )
        .bind(post)
        .bind(vec![1u8; 32])
        .bind(vec![1u8; 16])
        .execute(&pool)
        .await
        .unwrap();
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let favorite = format!("/posts/{post}/favorite");
        let vote = format!("/posts/{post}/vote");

        assert_eq!(
            app.post_form(&favorite, None, &[], "action=add")
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
        let response = app
            .post_form(
                &format!("{favorite}?q=cat"),
                Some(&alice),
                &[],
                "action=add",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        assert_eq!(
            response.location.as_deref(),
            Some(format!("/posts/{post}?q=cat").as_str())
        );
        let response = app
            .post_form(
                &vote,
                Some(&alice),
                &[("accept", "application/json")],
                "score=-1",
            )
            .await;
        let json: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "fav_count": 1, "favorited": true, "score": -1, "vote": -1 })
        );
        assert_eq!(
            app.post_form(&vote, Some(&alice), &[], "score=5")
                .await
                .status,
            StatusCode::BAD_REQUEST
        );

        let page = app.get(&format!("/posts/{post}"), Some(&alice)).await;
        assert!(
            page.body.contains("name=\"action\" value=\"remove\""),
            "{}",
            page.body
        );
        assert!(page.body.contains("class=\"score\">-1<"), "{}", page.body);
    }
}
