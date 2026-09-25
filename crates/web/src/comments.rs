//! Comments: the thread under each post, posting, editing and deleting
//! your own, and the list of recent comments.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::permissions::Permission;
use moekura_core::posts::PostStatus;
use moekura_db::comments::{self, Comment, Filter};
use moekura_db::posts::{self, Post};
use moekura_db::users;
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::posts::{CommentDraft, Extra, render_post, visibility};
use crate::templates::url_value;

/// Longest comment, in characters.
pub const MAX_LEN: usize = 10_000;

/// Comments shown under a post; older ones are on its comment list.
const THREAD_SIZE: i64 = 50;

/// Comments per page of the list.
const PAGE_SIZE: i64 = 25;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/comments", get(index))
        .route("/comments/{id}", get(show))
        .route("/comments/{id}/edit", get(edit_form).post(edit))
        .route("/comments/{id}/delete", post(delete))
        .route("/posts/{id}/comments", post(create))
}

/// Tidies text from a form or the API, as for wiki pages.
pub(crate) fn clean_body(body: &str) -> Result<String, AppError> {
    let body = body.replace("\r\n", "\n").trim_end().to_owned();
    if body.trim().is_empty() {
        return Err(AppError::Unprocessable("The comment is empty.".into()));
    }
    if body.chars().count() > MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "The comment is too long: the limit is {MAX_LEN} characters."
        )));
    }
    Ok(body)
}

/// Where a comment is shown: on its post's page.
pub(crate) fn url(comment: &Comment) -> String {
    format!("/posts/{}#comment-{}", comment.post_id, comment.id)
}

/// Post `id`, if `current` may see it.
async fn visible_post(state: &AppState, current: &CurrentUser, id: i64) -> Result<Post, AppError> {
    let post = posts::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !visibility(current).allows(&post) {
        return Err(AppError::NotFound);
    }
    Ok(post)
}

/// Whether `current` sees deleted comments.
fn sees_deleted(current: &CurrentUser) -> bool {
    current.can(Permission::ViewDeleted)
}

/// Comment `id` and its post, if `current` may see them.
async fn visible_comment(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<(Comment, Post), AppError> {
    let comment = comments::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    if comment.is_deleted && !sees_deleted(current) {
        return Err(AppError::NotFound);
    }
    let post = visible_post(state, current, comment.post_id).await?;
    Ok((comment, post))
}

/// Checks `current` may post a comment on `post` and returns their id.
pub(crate) async fn commenter(
    state: &AppState,
    current: &CurrentUser,
    post: &Post,
) -> Result<i64, AppError> {
    current.require(Permission::Comment)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    if post.status == PostStatus::Deleted {
        return Err(AppError::Unprocessable(
            "Deleted posts can't be commented on.".into(),
        ));
    }
    state.rate_limits.check_comment(user.id).await?;
    Ok(user.id)
}

/// Checks `current` may change `comment`: their own, not deleted.
pub(crate) fn check_author(current: &CurrentUser, comment: &Comment) -> Result<(), AppError> {
    current.require(Permission::Comment)?;
    let me = current.user.as_ref().map(|u| u.id);
    if comment.creator_id.is_none() || comment.creator_id != me || comment.is_deleted {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

/// A comment for templates.
fn comment_context(current: &CurrentUser, comment: &Comment) -> Value {
    let me = current.user.as_ref().map(|u| u.id);
    let own = comment.creator_id.is_some() && comment.creator_id == me && !comment.is_deleted;
    let may_comment = current.can(Permission::Comment);
    context! {
        id => comment.id,
        post_id => comment.post_id,
        url => url_value(&url(comment)),
        author => comment.creator_name,
        author_url => comment.creator_name.as_deref().map(|name| {
            url_value(&format!("/users/{}", url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>()))
        }),
        html => Value::from_safe_string(markup::render(&comment.body)),
        date => comment.created_at.date().to_string(),
        created_iso => comment.created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        edited => comment.edited_at.is_some(),
        deleted => comment.is_deleted,
        can_edit => own && may_comment,
        can_reply => may_comment && current.is_logged_in() && !comment.is_deleted,
    }
}

/// The comment thread for a post page: the latest comments and the form
/// for a new one, prefilled with `draft`.
pub(crate) async fn thread(
    state: &AppState,
    current: &CurrentUser,
    post: &Post,
    draft: Option<&CommentDraft>,
) -> Result<Value, AppError> {
    let db = state.db.primary();
    let with_deleted = sees_deleted(current);
    let shown = comments::for_post(db, post.id, with_deleted, THREAD_SIZE).await?;
    let visible_shown = shown.iter().filter(|c| !c.is_deleted).count() as i64;
    let older = i64::from(post.comment_count) - visible_shown;
    let list: Vec<Value> = shown.iter().map(|c| comment_context(current, c)).collect();
    let can_comment = current.is_logged_in()
        && current.can(Permission::Comment)
        && post.status != PostStatus::Deleted;
    Ok(context! {
        comments => list,
        count => post.comment_count,
        older => (older > 0).then_some(older),
        all_url => url_value(&format!("/comments?post_id={}", post.id)),
        can_comment => can_comment,
        login_needed => !current.is_logged_in(),
        draft => draft.map(|d| d.body.as_str()).unwrap_or_default(),
        error => draft.and_then(|d| d.error.as_deref()),
        max_len => MAX_LEN,
    })
}

/// The text to start a reply to comment `id` on post `post_id` with. The
/// post page checks the post is visible.
pub(crate) async fn reply_draft(
    state: &AppState,
    post_id: i64,
    id: i64,
) -> Result<Option<CommentDraft>, AppError> {
    let Some(comment) = comments::by_id(state.db.primary(), id).await? else {
        return Ok(None);
    };
    if comment.post_id != post_id || comment.is_deleted {
        return Ok(None);
    }
    let author = comment.creator_name.as_deref().unwrap_or("Someone");
    Ok(Some(CommentDraft {
        body: markup::quote(author, &comment.body),
        error: None,
    }))
}

#[derive(Debug, Deserialize)]
struct CommentForm {
    body: String,
}

async fn create(
    page: Page,
    Path(id): Path<i64>,
    Form(form): Form<CommentForm>,
) -> Result<Response, AppError> {
    let state = page.state();
    let post = visible_post(state, &page.current, id).await?;
    let refused = |error: AppError| {
        let message = match &error {
            AppError::TooManyRequests { retry_after_secs } => {
                format!("You're commenting too quickly. Try again in {retry_after_secs} seconds.")
            }
            other => other.public_message().to_owned(),
        };
        CommentDraft {
            body: form.body.clone(),
            error: Some(message),
        }
    };
    let result = async {
        let body = clean_body(&form.body)?;
        let user = commenter(state, &page.current, &post).await?;
        Ok::<_, AppError>((body, user))
    }
    .await;
    let (body, user) = match result {
        Ok(ok) => ok,
        Err(error @ (AppError::Unauthorized | AppError::Forbidden | AppError::Blocked(_))) => {
            return Err(error);
        }
        Err(error) => {
            let draft = refused(error);
            return render_post(
                &page,
                id,
                None,
                true,
                Extra {
                    comment: Some(draft),
                    ..Extra::default()
                },
            )
            .await;
        }
    };
    let comment_id = comments::create(state.db.primary(), post.id, user, &body).await?;
    tracing::info!(post = post.id, comment = comment_id, "comment posted");
    Ok(Redirect::to(&format!("/posts/{}#comment-{comment_id}", post.id)).into_response())
}

/// A comment's permanent link: on to its place on the post page.
async fn show(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let (comment, _) = visible_comment(page.state(), &page.current, id).await?;
    Ok(Redirect::to(&url(&comment)).into_response())
}

fn edit_context(comment: &Comment, body: &str, error: Option<String>) -> Value {
    context! {
        id => comment.id,
        post_id => comment.post_id,
        url => url_value(&url(comment)),
        body => body,
        error => error,
        max_len => MAX_LEN,
    }
}

async fn edit_form(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let (comment, _) = visible_comment(page.state(), &page.current, id).await?;
    check_author(&page.current, &comment)?;
    Ok(page.render(
        "comment_edit.html",
        edit_context(&comment, &comment.body, None),
    ))
}

async fn edit(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<CommentForm>,
) -> Result<Response, AppError> {
    let (comment, _) = visible_comment(page.state(), &page.current, id).await?;
    check_author(&page.current, &comment)?;
    let body = match clean_body(&form.body) {
        Ok(body) => body,
        Err(error) => {
            return Ok(page.render_with_status(
                error.status(),
                "comment_edit.html",
                edit_context(
                    &comment,
                    &form.body,
                    Some(error.public_message().to_owned()),
                ),
            ));
        }
    };
    comments::update(page.state().db.primary(), id, &body).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&url(&comment))).into_response())
}

async fn delete(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let (comment, _) = visible_comment(page.state(), &page.current, id).await?;
    check_author(&page.current, &comment)?;
    comments::set_deleted(page.state().db.primary(), id, true).await?;
    tracing::info!(comment = id, "comment deleted by its author");
    Ok(Redirect::to(&format!("/posts/{}#comments", comment.post_id)).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    post_id: Option<i64>,
    /// A user name.
    #[serde(default)]
    user: String,
    /// Comments older than this one.
    before: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let db = state.reader(&page.current);
    let creator = if query.user.is_empty() {
        None
    } else {
        Some(
            users::by_name(db, &query.user)
                .await?
                .ok_or(AppError::NotFound)?,
        )
    };
    let filter = Filter {
        post_id: query.post_id,
        creator_id: creator.as_ref().map(|u| u.id),
        with_deleted: sees_deleted(&page.current),
    };
    let mut found = comments::list(
        db,
        &visibility(&page.current),
        &filter,
        query.before,
        PAGE_SIZE + 1,
    )
    .await?;
    let has_next = found.len() > PAGE_SIZE as usize;
    found.truncate(PAGE_SIZE as usize);

    // Each comment beside its post's thumbnail, left out if blacklisted.
    let mut post_ids: Vec<i64> = found.iter().map(|c| c.post_id).collect();
    post_ids.dedup();
    let cards = crate::posts::grid(&page, db, &post_ids).await?;
    let rows: Vec<Value> = found
        .iter()
        .map(|c| {
            let card = cards
                .iter()
                .find(|(id, _)| *id == c.post_id)
                .map(|(_, card)| card);
            context! { comment => comment_context(&page.current, c), card => card }
        })
        .collect();
    let next_url = has_next.then(|| {
        let mut params = url::form_urlencoded::Serializer::new(String::new());
        if let Some(post_id) = query.post_id {
            params.append_pair("post_id", &post_id.to_string());
        }
        if !query.user.is_empty() {
            params.append_pair("user", &query.user);
        }
        if let Some(last) = found.last() {
            params.append_pair("before", &last.id.to_string());
        }
        url_value(&format!("/comments?{}", params.finish()))
    });
    Ok(page.render(
        "comments.html",
        context! {
            rows => rows,
            post_id => query.post_id,
            user => creator.map(|u| u.name),
            next_url => next_url,
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(
            test_state(pool).await,
            routes().merge(crate::posts::routes()),
        )
    }

    async fn post(pool: &PgPool, status: &str) -> i64 {
        let post: i64 =
            sqlx::query_scalar("INSERT INTO posts (rating, status) VALUES ('g', $1) RETURNING id")
                .bind(status)
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
             VALUES ($1, sha256($1::text::bytea), substring(sha256($1::text::bytea) FROM 1 FOR 16), 'png', 10, 10, 1, 'original/aa/aa/x.png')",
        )
        .bind(post)
        .execute(pool)
        .await
        .unwrap();
        post
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn post_edit_and_delete_comments(pool: PgPool) {
        let app = app(&pool).await;
        let post = post(&pool, "active").await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let create = format!("/posts/{post}/comments");

        let page = app.get(&format!("/posts/{post}"), None).await;
        assert!(page.body.contains("Log in</a> to comment"), "{}", page.body);
        assert_eq!(
            app.post_form(&create, None, &[], "body=hi").await.status,
            StatusCode::UNAUTHORIZED
        );

        let posted = app
            .post_form(&create, Some(&alice), &[], "body=Nice+%5Bb%5Dcat%5B%2Fb%5D")
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let location = posted.location.unwrap();
        let id: i64 = location
            .rsplit("#comment-")
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(location, format!("/posts/{post}#comment-{id}"));

        let page = app.get(&format!("/posts/{post}"), None).await;
        assert!(
            page.body.contains("Nice <strong>cat</strong>"),
            "{}",
            page.body
        );
        assert!(page.body.contains(&format!("id=\"comment-{id}\"")));
        assert_eq!(
            app.get(&format!("/comments/{id}"), None).await.location,
            Some(location.clone())
        );

        // Empty comments come back with the form.
        let empty = app.post_form(&create, Some(&alice), &[], "body=+").await;
        assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            empty.body.contains("The comment is empty."),
            "{}",
            empty.body
        );

        // Replies quote the comment.
        let reply = app
            .get(&format!("/posts/{post}?reply={id}"), Some(&bob))
            .await;
        assert!(
            reply
                .body
                .contains("[quote]\nalice said:\n\nNice [b]cat[&#x2f;b]\n[&#x2f;quote]"),
            "{}",
            reply.body
        );

        // Only the author edits or deletes.
        let edit = format!("/comments/{id}/edit");
        assert_eq!(
            app.post_form(&edit, Some(&bob), &[], "body=mine")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert!(
            app.get(&edit, Some(&alice))
                .await
                .body
                .contains("Nice [b]cat[&#x2f;b]</textarea>")
        );
        let edited = app
            .post_form(&edit, Some(&alice), &[], "body=Nice+dog")
            .await;
        assert_eq!(edited.status, StatusCode::SEE_OTHER);
        let page = app.get(&format!("/posts/{post}"), None).await;
        assert!(page.body.contains("Nice dog"));
        assert!(page.body.contains("edited"));

        let delete = format!("/comments/{id}/delete");
        assert_eq!(
            app.post(&delete, Some(&bob), &[]).await.status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.post(&delete, Some(&alice), &[]).await.status,
            StatusCode::SEE_OTHER
        );
        let page = app.get(&format!("/posts/{post}"), None).await;
        assert!(!page.body.contains("Nice dog"));
        assert_eq!(
            app.get(&format!("/comments/{id}"), None).await.status,
            StatusCode::NOT_FOUND
        );
        let staff = session_for(&pool, "jan", SystemRole::Janitor).await;
        let page = app.get(&format!("/posts/{post}"), Some(&staff)).await;
        assert!(page.body.contains("Nice dog"), "staff see deleted comments");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn hidden_and_deleted_posts(pool: PgPool) {
        let app = app(&pool).await;
        let pending = post(&pool, "pending").await;
        let deleted = post(&pool, "deleted").await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        assert_eq!(
            app.post_form(
                &format!("/posts/{pending}/comments"),
                Some(&alice),
                &[],
                "body=hi"
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );
        let staff = session_for(&pool, "jan", SystemRole::Janitor).await;
        let refused = app
            .post_form(
                &format!("/posts/{deleted}/comments"),
                Some(&staff),
                &[],
                "body=hi",
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            refused.body.contains("can&#x27;t be commented on"),
            "{}",
            refused.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn lists_recent_comments(pool: PgPool) {
        let app = app(&pool).await;
        let visible = post(&pool, "active").await;
        let pending = post(&pool, "pending").await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let alice_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'alice'")
            .fetch_one(&pool)
            .await
            .unwrap();
        for i in 0..30 {
            comments::create(&pool, visible, alice_id, &format!("number {i}."))
                .await
                .unwrap();
        }
        comments::create(&pool, pending, alice_id, "secret")
            .await
            .unwrap();

        let list = app.get("/comments", None).await;
        assert_eq!(list.status, StatusCode::OK);
        assert!(list.body.contains("number 29."), "{}", list.body);
        assert!(!list.body.contains("number 4."));
        assert!(!list.body.contains("secret"));
        assert!(list.body.contains(&format!("href=\"/posts/{visible}\"")));
        let next = list
            .body
            .split("rel=\"next\" href=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap()
            .replace("&amp;", "&");
        let older = app.get(&next, None).await;
        assert!(older.body.contains("number 4."), "{}", older.body);
        assert!(older.body.contains("number 0."));

        let by_user = app.get("/comments?user=alice", Some(&alice)).await;
        assert!(by_user.body.contains("number 29."));
        assert_eq!(
            app.get("/comments?user=nobody", None).await.status,
            StatusCode::NOT_FOUND
        );

        // The post page shows the latest 50 and links to the rest.
        for i in 30..55 {
            comments::create(&pool, visible, alice_id, &format!("number {i}."))
                .await
                .unwrap();
        }
        let page = app.get(&format!("/posts/{visible}"), None).await;
        assert!(page.body.contains("number 54."));
        assert!(!page.body.contains("number 4."));
        assert!(page.body.contains("5 older comments"), "{}", page.body);
    }
}
