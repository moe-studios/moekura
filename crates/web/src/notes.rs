//! Notes on posts: the overlay on post pages, the history page, and the
//! checks the editor (through the API) and history share.

use axum::Router;
use axum::extract::Path;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::notes::{MAX_LEN, NoteBox};
use moekura_core::permissions::Permission;
use moekura_core::posts::PostStatus;
use moekura_db::notes::{self, Changes, Note, SaveError};
use moekura_db::{media, posts};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::posts::visibility;

/// Versions shown on a post's note history.
const HISTORY_SIZE: i64 = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/posts/{id}/notes/history", get(history))
        .route("/notes/{id}/revert/{version}", post(revert))
}

/// Post `id` and its image's size, if `current` may see it.
pub(crate) async fn visible_post(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<(posts::Post, i32, i32), AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.db.primary();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if !visibility(current).allows(&post) {
        return Err(AppError::NotFound);
    }
    let asset = media::for_post(db, id).await?.ok_or(AppError::NotFound)?;
    Ok((post, asset.width, asset.height))
}

/// Note `id` and its image's size, if `current` may see the post and,
/// when deleted, the note.
pub(crate) async fn visible_note(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<(Note, i32, i32), AppError> {
    let note = notes::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    let (_, width, height) = visible_post(state, current, note.post_id).await?;
    Ok((note, width, height))
}

/// Checks `current` may change notes on `post`.
pub(crate) fn check_editor(current: &CurrentUser, post: &posts::Post) -> Result<(), AppError> {
    current.require(Permission::EditNotes)?;
    if !current.is_logged_in() {
        return Err(AppError::Unauthorized);
    }
    if post.status == PostStatus::Deleted {
        return Err(AppError::Unprocessable(
            "Notes on deleted posts can't be changed.".into(),
        ));
    }
    Ok(())
}

/// A note's text, checked.
pub(crate) fn clean_body(body: &str) -> Result<String, AppError> {
    let body = moekura_core::notes::clean_body(body)
        .ok_or_else(|| AppError::Unprocessable("The note is empty.".into()))?;
    if body.chars().count() > MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "The note is too long: the limit is {MAX_LEN} characters."
        )));
    }
    Ok(body)
}

/// A box, checked against a `width` × `height` image.
pub(crate) fn fit(note_box: NoteBox, width: i32, height: i32) -> Result<NoteBox, AppError> {
    note_box
        .fit(width, height)
        .map_err(|e| AppError::Unprocessable(e.to_string()))
}

pub(crate) fn conflict() -> AppError {
    AppError::Conflict(
        "Someone else changed this note while you were editing it. Reload to see their change."
            .into(),
    )
}

/// Changes note `id` as `current`, checking everything; returns its
/// version afterwards.
pub(crate) async fn update(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    mut changes: Changes,
    base: Option<i32>,
) -> Result<i32, AppError> {
    let (note, width, height) = visible_note(state, current, id).await?;
    let post = posts::by_id(state.db.primary(), note.post_id)
        .await?
        .ok_or(AppError::NotFound)?;
    check_editor(current, &post)?;
    if let Some(body) = &changes.body {
        changes.body = Some(clean_body(body)?);
    }
    if let Some(note_box) = changes.note_box {
        changes.note_box = Some(fit(note_box, width, height)?);
    }
    let user = current.user.as_ref().map(|u| u.id);
    notes::update(state.db.primary(), id, &changes, user, base)
        .await
        .map_err(|e| match e {
            SaveError::Conflict { .. } => conflict(),
            SaveError::NotFound => AppError::NotFound,
            SaveError::Db(e) => e.into(),
        })
}

/// Notes for templates. Boxes stay in the original's pixels: the overlay
/// is an SVG with the image's size as its view box, stretched over the
/// image however large it's shown.
pub(crate) fn note_contexts(notes: &[Note]) -> Vec<Value> {
    notes
        .iter()
        .map(|n| {
            context! {
                id => n.id,
                version => n.version,
                x => n.x, y => n.y, width => n.width, height => n.height,
                body => n.body,
                html => Value::from_safe_string(markup::render(&n.body)),
            }
        })
        .collect()
}

async fn history(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let (post, _, _) = visible_post(page.state(), &page.current, id).await?;
    let db = page.state().reader(&page.current);
    let versions = notes::versions_for_post(db, id, HISTORY_SIZE).await?;
    let can_revert = page.current.is_logged_in()
        && page.current.can(Permission::EditNotes)
        && post.status != PostStatus::Deleted;
    // The newest version of each note is its current state.
    let mut seen = Vec::new();
    let rows: Vec<Value> = versions
        .iter()
        .map(|v| {
            let current = !seen.contains(&v.note_id);
            seen.push(v.note_id);
            context! {
                note_id => v.note_id,
                version => v.version,
                date => v.created_at.date().to_string(),
                updater => v.updater_name,
                box => format!("{}×{} at {},{}", v.width, v.height, v.x, v.y),
                body => v.body,
                active => v.is_active,
                current => current,
                can_revert => can_revert && !current,
            }
        })
        .collect();
    Ok(page.render(
        "note_history.html",
        context! { post_id => id, versions => rows },
    ))
}

async fn revert(
    page: Page,
    jar: CookieJar,
    Path((id, version)): Path<(i64, i32)>,
) -> Result<Response, AppError> {
    let old = notes::version(page.state().db.primary(), id, version)
        .await?
        .ok_or(AppError::NotFound)?;
    update(page.state(), &page.current, id, old.changes(), None).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/posts/{}/notes/history", old.post_id)),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    pub(crate) async fn post(pool: &PgPool, status: &str) -> i64 {
        let post: i64 =
            sqlx::query_scalar("INSERT INTO posts (rating, status) VALUES ('g', $1) RETURNING id")
                .bind(status)
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
             VALUES ($1, sha256($1::text::bytea), substring(sha256($1::text::bytea) FROM 1 FOR 16), 'png', 200, 100, 1, 'original/aa/aa/x.png')",
        )
        .bind(post)
        .execute(pool)
        .await
        .unwrap();
        post
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn notes_show_over_the_image(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, crate::posts::routes());
        let post = post(&pool, "active").await;
        let bare = app.get(&format!("/posts/{post}"), None).await;
        assert!(!bare.body.contains("class=\"notes\""));
        let note_box = NoteBox {
            x: 10,
            y: 20,
            width: 50,
            height: 30,
        };
        notes::create(&pool, post, note_box, "Hello <there> [b]you[/b]", None)
            .await
            .unwrap();
        let page = app.get(&format!("/posts/{post}"), None).await;
        assert!(
            page.body.contains("viewBox=\"0 0 200 100\""),
            "{}",
            page.body
        );
        assert!(
            page.body
                .contains("x=\"10\" y=\"20\" width=\"50\" height=\"30\"")
        );
        // Escaped in the tooltip, rendered in the list.
        assert!(
            page.body
                .contains("<title>Hello &lt;there&gt; [b]you[&#x2f;b]</title>")
        );
        assert!(
            page.body
                .contains("Hello &lt;there&gt; <strong>you</strong>")
        );
        assert!(page.body.contains("Notes (1)"));
        assert!(page.body.contains(&format!("/posts/{post}/notes/history")));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn history_and_revert(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            routes().merge(crate::posts::routes()),
        );
        let post = post(&pool, "active").await;
        let note_box = NoteBox {
            x: 10,
            y: 10,
            width: 50,
            height: 20,
        };
        let id = notes::create(&pool, post, note_box, "Hello", None)
            .await
            .unwrap();
        let changes = Changes {
            body: Some("Goodbye".into()),
            ..Changes::default()
        };
        notes::update(&pool, id, &changes, None, None)
            .await
            .unwrap();

        let history = app.get(&format!("/posts/{post}/notes/history"), None).await;
        assert_eq!(history.status, StatusCode::OK);
        assert!(
            history.body.contains("Goodbye") && history.body.contains("Hello"),
            "{}",
            history.body
        );
        assert!(!history.body.contains("/revert/"), "visitors can't revert");

        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let page = app
            .get(&format!("/posts/{post}/notes/history"), Some(&alice))
            .await;
        assert!(page.body.contains(&format!("/notes/{id}/revert/1")));
        let reverted = app
            .post(&format!("/notes/{id}/revert/1"), Some(&alice), &[])
            .await;
        assert_eq!(reverted.status, StatusCode::SEE_OTHER, "{}", reverted.body);
        assert_eq!(
            notes::by_id(&pool, id).await.unwrap().unwrap().body,
            "Hello"
        );
    }
}
