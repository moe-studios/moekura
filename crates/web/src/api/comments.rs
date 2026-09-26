//! Comments, their votes and reports.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use moekura_core::markup;
use moekura_core::permissions::Permission;
use moekura_db::comments::{self, Comment, Filter};
use moekura_db::users;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::comments::{
    CommentScore, check_author, clean_body, commenter, moderate, report_comment, sees_deleted,
    visible_comment, vote_on,
};
use crate::error::{AppError, ErrorBody};
use crate::posts::visibility;

/// Comments per page of a list.
const PAGE_SIZE: i64 = 50;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiComment {
    pub id: i64,
    pub post_id: i64,
    /// The author's name, unless their account is gone.
    pub creator: Option<String>,
    /// The text, in the site's wiki markup.
    pub body: String,
    /// `body` as HTML, the way the site shows it.
    pub html: String,
    pub score: i32,
    /// Only staff see deleted comments.
    pub is_deleted: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub edited_at: Option<OffsetDateTime>,
}

impl From<Comment> for ApiComment {
    fn from(c: Comment) -> Self {
        Self {
            html: markup::render(&c.body),
            id: c.id,
            post_id: c.post_id,
            creator: c.creator_name,
            body: c.body,
            score: c.score,
            is_deleted: c.is_deleted,
            created_at: c.created_at,
            edited_at: c.edited_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CommentList {
    /// Newest first.
    pub comments: Vec<ApiComment>,
    /// `before` for the next page; absent on the last one.
    pub next_before: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// Only comments on this post.
    post_id: Option<i64>,
    /// Only comments by this user (a name).
    user: Option<String>,
    /// Only comments older than this one, for the next page.
    before: Option<i64>,
}

/// List comments.
///
/// Newest first, 50 at a time, on posts you can see. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/comments",
    operation_id = "list_comments",
    tag = "comments",
    params(ListParams),
    responses((status = 200, body = CommentList), (status = 404, body = ErrorBody, description = "No such user")),
)]
pub(crate) async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<ListParams>,
) -> Result<Json<CommentList>, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let creator_id = match params.user.as_deref() {
        Some(name) => Some(
            users::by_name(db, name)
                .await?
                .ok_or(AppError::NotFound)?
                .id,
        ),
        None => None,
    };
    let filter = Filter {
        post_id: params.post_id,
        creator_id,
        with_deleted: sees_deleted(&current),
    };
    let mut found = comments::list(
        db,
        &visibility(&current),
        &filter,
        params.before,
        PAGE_SIZE + 1,
    )
    .await?;
    let has_next = found.len() > PAGE_SIZE as usize;
    found.truncate(PAGE_SIZE as usize);
    let next_before = has_next.then(|| found.last().map(|c| c.id)).flatten();
    Ok(Json(CommentList {
        comments: found.into_iter().map(ApiComment::from).collect(),
        next_before,
    }))
}

/// Get a comment.
///
/// Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/comments/{id}",
    operation_id = "get_comment",
    tag = "comments",
    params(("id" = i64, Path, description = "Comment number")),
    responses((status = 200, body = ApiComment), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<ApiComment>, AppError> {
    current.require(Permission::ViewPosts)?;
    let (comment, _) = visible_comment(&state, &current, id).await?;
    Ok(Json(comment.into()))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentText {
    /// In the site's wiki markup; at most 10,000 characters.
    body: String,
}

/// Comment on a post.
///
/// Needs `comment`. Rate limited; deleted posts can't be commented on.
#[utoipa::path(
    post,
    path = "/posts/{id}/comments",
    operation_id = "create_comment",
    tag = "comments",
    params(("id" = i64, Path, description = "Post number")),
    request_body = CommentText,
    responses(
        (status = 201, body = ApiComment),
        (status = 404, body = ErrorBody),
        (status = 422, body = ErrorBody, description = "The text is empty or too long, or the post is deleted"),
        (status = 429, body = ErrorBody),
    ),
)]
pub(crate) async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(text): Json<CommentText>,
) -> Result<(StatusCode, Json<ApiComment>), AppError> {
    current.require(Permission::ViewPosts)?;
    let post = moekura_db::posts::by_id(state.db.primary(), id)
        .await?
        .filter(|p| visibility(&current).allows(p))
        .ok_or(AppError::NotFound)?;
    let body = clean_body(&text.body)?;
    let user = commenter(&state, &current, &post).await?;
    let db = state.db.primary();
    let comment_id = comments::create(db, id, user, &body).await?;
    crate::webhooks::emit_comment(&state, comment_id).await;
    let comment = comments::by_id(db, comment_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, Json(comment.into())))
}

/// Change your comment.
///
/// Needs `comment`; only the author changes a comment.
#[utoipa::path(
    put,
    path = "/comments/{id}",
    operation_id = "update_comment",
    tag = "comments",
    params(("id" = i64, Path, description = "Comment number")),
    request_body = CommentText,
    responses((status = 200, body = ApiComment), (status = 403, body = ErrorBody), (status = 422, body = ErrorBody)),
)]
pub(crate) async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(text): Json<CommentText>,
) -> Result<Json<ApiComment>, AppError> {
    let (comment, _) = visible_comment(&state, &current, id).await?;
    check_author(&current, &comment)?;
    let body = clean_body(&text.body)?;
    let db = state.db.primary();
    comments::update(db, id, &body).await?;
    let comment = comments::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    Ok(Json(comment.into()))
}

/// Delete your comment.
///
/// Needs `comment`; only the author deletes a comment this way (staff
/// hide it instead).
#[utoipa::path(
    delete,
    path = "/comments/{id}",
    operation_id = "delete_comment",
    tag = "comments",
    params(("id" = i64, Path, description = "Comment number")),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn delete(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let (comment, _) = visible_comment(&state, &current, id).await?;
    check_author(&current, &comment)?;
    comments::set_deleted(state.db.primary(), id, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentVote {
    /// 1, -1, or 0 to take your vote back.
    score: i16,
}

/// Vote on a comment.
///
/// Needs `vote`; not on your own comments.
#[utoipa::path(
    put,
    path = "/comments/{id}/vote",
    operation_id = "vote_on_comment",
    tag = "comments",
    params(("id" = i64, Path, description = "Comment number")),
    request_body = CommentVote,
    responses((status = 200, body = CommentScore), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)),
)]
pub(crate) async fn vote(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(vote): Json<CommentVote>,
) -> Result<Json<CommentScore>, AppError> {
    Ok(Json(vote_on(&state, &current, id, vote.score).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Reason {
    /// At most 2,000 characters.
    #[serde(default)]
    reason: String,
}

/// Report a comment.
///
/// Needs `flag`. Staff find reported comments under Moderation.
#[utoipa::path(
    post,
    path = "/comments/{id}/report",
    operation_id = "report_comment",
    tag = "comments",
    params(("id" = i64, Path, description = "Comment number")),
    request_body = Reason,
    responses((status = 204), (status = 400, body = ErrorBody, description = "No reason, or already reported")),
)]
pub(crate) async fn report(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(reason): Json<Reason>,
) -> Result<StatusCode, AppError> {
    report_comment(&state, &current, id, &reason.reason).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Hide a comment.
///
/// Needs `moderate_comments`. Upholds the comment's open reports; logged.
#[utoipa::path(
    post,
    path = "/comments/{id}/hide",
    operation_id = "hide_comment",
    tag = "moderation",
    params(("id" = i64, Path, description = "Comment number")),
    request_body = Reason,
    responses((status = 200, body = ApiComment), (status = 400, body = ErrorBody, description = "Already hidden")),
)]
pub(crate) async fn hide(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(reason): Json<Reason>,
) -> Result<Json<ApiComment>, AppError> {
    moderate(&state, &current, id, true, &reason.reason).await?;
    let comment = comments::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(comment.into()))
}

/// Restore a hidden or deleted comment.
///
/// Needs `moderate_comments`; logged.
#[utoipa::path(
    post,
    path = "/comments/{id}/restore",
    operation_id = "restore_comment",
    tag = "moderation",
    params(("id" = i64, Path, description = "Comment number")),
    responses((status = 200, body = ApiComment), (status = 400, body = ErrorBody, description = "Not hidden")),
)]
pub(crate) async fn restore(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<ApiComment>, AppError> {
    moderate(&state, &current, id, false, "").await?;
    let comment = comments::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(comment.into()))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::json;
    use sqlx::PgPool;

    use crate::api::test_support::{app, json, upload};
    use crate::test_support::{fixture, session_for};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn comments(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        let post = upload(&app, &jan, &fixture::png(20, 20), "cat").await;

        let created = app
            .json(
                "POST",
                &format!("/api/v1/posts/{post}/comments"),
                Some(&alice),
                Some(json!({ "body": "Hi [b]there[/b]" })),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let comment = json(&created.body);
        let id = comment["id"].as_i64().unwrap();
        assert_eq!(comment["html"], json!("<p>Hi <strong>there</strong></p>"));
        assert_eq!(comment["creator"], json!("alice"));

        let empty = app
            .json(
                "POST",
                &format!("/api/v1/posts/{post}/comments"),
                Some(&alice),
                Some(json!({ "body": " " })),
            )
            .await;
        assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);

        let voted = app
            .json(
                "PUT",
                &format!("/api/v1/comments/{id}/vote"),
                Some(&bob),
                Some(json!({ "score": 1 })),
            )
            .await;
        assert_eq!(json(&voted.body), json!({ "score": 1, "vote": 1 }));
        let edited = app
            .json(
                "PUT",
                &format!("/api/v1/comments/{id}"),
                Some(&bob),
                Some(json!({ "body": "mine" })),
            )
            .await;
        assert_eq!(edited.status, StatusCode::FORBIDDEN);
        app.json(
            "PUT",
            &format!("/api/v1/comments/{id}"),
            Some(&alice),
            Some(json!({ "body": "Edited" })),
        )
        .await;

        let list = json(
            &app.get(&format!("/api/v1/comments?post_id={post}"), None)
                .await
                .body,
        );
        assert_eq!(list["comments"][0]["body"], json!("Edited"));
        assert_eq!(list["comments"][0]["score"], json!(1));
        assert_eq!(list["next_before"], json!(null));
        let post_json = json(&app.get(&format!("/api/v1/posts/{post}"), None).await.body);
        assert_eq!(post_json["comment_count"], json!(1));

        let reported = app
            .json(
                "POST",
                &format!("/api/v1/comments/{id}/report"),
                Some(&bob),
                Some(json!({ "reason": "rude" })),
            )
            .await;
        assert_eq!(reported.status, StatusCode::NO_CONTENT, "{}", reported.body);
        let hidden = app
            .json(
                "POST",
                &format!("/api/v1/comments/{id}/hide"),
                Some(&jan),
                Some(json!({ "reason": "rude" })),
            )
            .await;
        assert_eq!(json(&hidden.body)["is_deleted"], json!(true));
        assert_eq!(
            app.get(&format!("/api/v1/comments/{id}"), None)
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        let restored = app
            .json(
                "POST",
                &format!("/api/v1/comments/{id}/restore"),
                Some(&jan),
                None,
            )
            .await;
        assert_eq!(json(&restored.body)["is_deleted"], json!(false));
        assert_eq!(
            app.json(
                "DELETE",
                &format!("/api/v1/comments/{id}"),
                Some(&alice),
                None
            )
            .await
            .status,
            StatusCode::NO_CONTENT
        );
    }
}
