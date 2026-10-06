//! Favoriting and voting on posts. Plain forms that redirect back to the
//! post; the page script sends them with `Accept: application/json` and
//! gets the new counts instead, to skip the reload.

use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::http::header::ACCEPT;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum::{Form, Json, Router};
use moekura_core::permissions::Permission;
use moekura_db::{favorites, posts};
use serde::{Deserialize, Serialize};

use sqlx::PgPool;

use crate::AppState;
use crate::auth::CurrentUser;
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
    /// `add` or `remove`. Not named `action`: a field by that name would
    /// shadow `form.action` in the page's DOM.
    favorite: String,
}

#[derive(Debug, Deserialize)]
struct VoteForm {
    /// `1`, `-1`, or `0` to take the vote back.
    score: i16,
}

/// A post's counts and the viewer's part in them.
#[derive(Debug, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct Reactions {
    pub fav_count: i32,
    /// Whether you have favorited the post.
    pub favorited: bool,
    pub score: i32,
    /// Your vote: 1, -1, or 0 for none.
    pub vote: i16,
}

/// Checks `current` may act on post `id` and returns their id.
pub(crate) async fn user_for(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    permission: Permission,
) -> Result<i64, AppError> {
    current.require(permission)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let post = posts::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !visibility(current).allows(&post) {
        return Err(AppError::NotFound);
    }
    Ok(user.id)
}

/// Post `id`'s reactions as `user` sees them.
pub(crate) async fn reactions(db: &PgPool, id: i64, user: i64) -> Result<Reactions, AppError> {
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    Ok(Reactions {
        fav_count: post.fav_count,
        favorited: favorites::exists(db, user, id).await?,
        score: post.score,
        vote: favorites::vote_of(db, user, id).await?,
    })
}

/// Records `user`'s vote on post `id`. Uploaders don't vote on their own
/// posts, as nobody does on their own comments, but may take back a vote
/// they gave one.
pub(crate) async fn vote_on(db: &PgPool, id: i64, user: i64, score: i16) -> Result<(), AppError> {
    if !(-1..=1).contains(&score) {
        return Err(AppError::BadRequest("A vote is 1, -1 or 0".into()));
    }
    if score != 0 {
        let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
        if post.uploader_id == Some(user) {
            return Err(AppError::Unprocessable(
                "You can't vote on your own post.".into(),
            ));
        }
    }
    favorites::vote(db, user, id, score).await?;
    Ok(())
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
    Ok(Json(reactions(page.state().db.primary(), id, user).await?).into_response())
}

async fn favorite(
    page: Page,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(back): Query<BackQuery>,
    Form(form): Form<FavoriteForm>,
) -> Result<Response, AppError> {
    let user = user_for(page.state(), &page.current, id, Permission::Favorite).await?;
    let db = page.state().db.primary();
    match form.favorite.as_str() {
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
    let user = user_for(page.state(), &page.current, id, Permission::Vote).await?;
    vote_on(page.state().db.primary(), id, user, form.score).await?;
    respond(&page, &headers, id, user, &back).await
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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
            app.post_form(&favorite, None, &[], "favorite=add")
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
        let response = app
            .post_form(
                &format!("{favorite}?q=cat"),
                Some(&alice),
                &[],
                "favorite=add",
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
            page.body.contains("name=\"favorite\" value=\"remove\""),
            "{}",
            page.body
        );
        assert!(page.body.contains("class=\"score\">-1<"), "{}", page.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn uploaders_do_not_vote_on_their_own_posts(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            crate::danbooru::test_support::routes()
                .merge(super::routes())
                .merge(crate::edit::routes())
                .merge(crate::api::routes(max)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let id = crate::danbooru::test_support::upload(&app, &alice, 20, "cat").await;
        let score = async || -> i32 {
            sqlx::query_scalar("SELECT score FROM posts WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        let vote = format!("/posts/{id}/vote");
        let api = format!("/api/v1/posts/{id}/vote");
        let edit = |tags: &str| format!("old_tags=cat&tags={tags}&rating=s");

        // Not on the site, through the API, Danbooru apps or metatags.
        let web = app.post_form(&vote, Some(&alice), &[], "score=1").await;
        assert_eq!(web.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(web.body.contains("your own post"), "{}", web.body);
        let json = app
            .json(
                "PUT",
                &api,
                Some(&alice),
                Some(serde_json::json!({ "score": 1 })),
            )
            .await;
        assert_eq!(
            json.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            json.body
        );
        let danbooru = app
            .json(
                "POST",
                &format!("/posts/{id}/votes.json?score=1"),
                Some(&alice),
                None,
            )
            .await;
        assert_eq!(danbooru.status, StatusCode::UNPROCESSABLE_ENTITY);
        let tagged = app
            .post_form(
                &format!("/posts/{id}/edit"),
                Some(&alice),
                &[],
                &edit("cat+dog+upvote"),
            )
            .await;
        assert_eq!(tagged.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(tagged.body.contains("your own post"), "{}", tagged.body);
        // Refused before the edit was saved.
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.tag_ids.len(), 1);
        assert_eq!(score().await, 0);
        // Nor on an upload, which is theirs.
        let fields = vec![
            ("rating", "s".to_owned()),
            ("tags", "dog upvote".to_owned()),
        ];
        let png = crate::test_support::fixture::png(24, 20);
        let upload = app
            .post_multipart("/upload", Some(&alice), &fields, Some(("a.png", &png)))
            .await;
        assert_eq!(upload.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(upload.body.contains("your own post"), "{}", upload.body);
        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await.body;
        assert!(!page.contains("data-vote=\"up\""), "{page}");

        // Others vote on it, by hand or by metatag.
        let response = app.post_form(&vote, Some(&bob), &[], "score=1").await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        let page = app.get(&format!("/posts/{id}"), Some(&bob)).await.body;
        assert!(page.contains("data-vote=\"up\""), "{page}");
        let tagged = app
            .post_form(
                &format!("/posts/{id}/edit"),
                Some(&bob),
                &[],
                &edit("cat+downvote"),
            )
            .await;
        assert_eq!(tagged.status, StatusCode::SEE_OTHER, "{}", tagged.body);
        assert_eq!(score().await, -1);

        // A vote given before can still be taken back.
        sqlx::query(
            "INSERT INTO post_votes (user_id, post_id, score)
             SELECT id, $1, 1 FROM users WHERE name = 'alice'",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(score().await, 0);
        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await.body;
        assert!(page.contains("data-vote=\"up\""), "{page}");
        let response = app.post_form(&vote, Some(&alice), &[], "score=0").await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        let taken_back = app
            .json(
                "DELETE",
                &format!("/post_votes/{id}.json"),
                Some(&alice),
                None,
            )
            .await;
        assert_eq!(taken_back.status, StatusCode::NO_CONTENT);
        assert_eq!(score().await, -1);
    }
}
