//! Moderating many posts at once, in the background: deleting every
//! upload of a user, from their moderation record.

use axum::Router;
use axum::extract::{Form, Path};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::jobs::PostBatch as PostBatchJob;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::post_batches::{self, PostBatch};
use moekura_db::users::{self, User};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::moderation::{ReasonForm, check_reason};
use crate::pages::Page;

pub fn routes() -> Router<AppState> {
    Router::new().route("/users/{name}/delete-uploads", post(delete_uploads_form))
}

/// Whether `current` may delete every upload of `user`: with
/// `delete_posts`, and only for users ranked below them.
pub(crate) fn may_delete_uploads(state: &AppState, current: &CurrentUser, user: &User) -> bool {
    current.can(Permission::DeletePosts)
        && state
            .site
            .get()
            .role(user.role_id)
            .is_none_or(|r| current.role.outranks(r))
}

/// Starts deleting every upload of `user` that can be deleted, as
/// `current`, with `reason`; returns the batch.
pub(crate) async fn delete_uploads(
    state: &AppState,
    current: &CurrentUser,
    user: &User,
    reason: &str,
) -> Result<PostBatch, AppError> {
    current.require(Permission::DeletePosts)?;
    if !may_delete_uploads(state, current, user) {
        return Err(AppError::Forbidden);
    }
    let reason = check_reason(reason)?;
    // The reason is what each post shows in its place.
    if reason.is_empty() {
        return Err(AppError::BadRequest(
            "Say why the posts are being deleted".into(),
        ));
    }
    let actor = current.user.as_ref().map(|u| u.id);
    let db = state.db.primary();
    let mut tx = db.begin().await?;
    // One at a time per user.
    sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    if post_batches::deletion_open(&mut *tx, user.id).await? {
        return Err(AppError::BadRequest(format!(
            "{}'s uploads are already being deleted",
            user.name
        )));
    }
    let total = post_batches::count_deletable(&mut *tx, user.id).await?;
    if total == 0 {
        return Err(AppError::BadRequest(format!(
            "{} has no uploads left to delete",
            user.name
        )));
    }
    let id = post_batches::create_deletion(
        &mut *tx,
        actor,
        user.id,
        reason,
        current.can(Permission::LockPosts),
        total,
    )
    .await?;
    moekura_db::jobs::enqueue(&mut tx, &PostBatchJob { id }).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::DeleteUploads)
            .user(user.id)
            .reason(reason)
            .details(serde_json::json!({ "batch": id, "posts": total })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(
        id,
        user = user.name,
        posts = total,
        "deleting a user's uploads"
    );
    post_batches::by_id(db, id).await?.ok_or(AppError::NotFound)
}

/// A batch as pages show it.
pub(crate) fn batch_context(b: &PostBatch) -> Value {
    context! {
        id => b.id,
        by => b.creator_name,
        user => b.user_name,
        reason => b.reason,
        status => b.status,
        total => b.total,
        done => b.done,
        skipped => b.skipped,
        failed => b.failed,
        error => b.error,
        when => crate::dates::day(b.created_at),
    }
}

#[derive(Debug, Deserialize)]
struct DeleteUploadsForm {
    #[serde(flatten)]
    reason: ReasonForm,
    /// The box saying the uploads are to go.
    #[serde(default)]
    confirm: Option<String>,
}

async fn delete_uploads_form(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(form): Form<DeleteUploadsForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::DeletePosts)?;
    let user = users::by_name(page.state().db.primary(), &name)
        .await?
        .ok_or(AppError::NotFound)?;
    if form.confirm.is_none() {
        return Err(AppError::BadRequest(
            "Tick the box to confirm the uploads are to be deleted".into(),
        ));
    }
    delete_uploads(page.state(), &page.current, &user, &form.reason.reason()).await?;
    Ok((
        flash::set(jar, Flash::Queued),
        Redirect::to(&crate::user_moderation::url(&user.name)),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    async fn user_id(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn deleting_a_users_uploads(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::user_moderation::routes()),
        );
        let spammer = session_for(&pool, "spammer", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let spammer_id = user_id(&pool, "spammer").await;
        for status in ["active", "pending", "flagged", "deleted"] {
            sqlx::query("INSERT INTO posts (rating, status, uploader_id) VALUES ('g', $1, $2)")
                .bind(status)
                .bind(spammer_id)
                .execute(&pool)
                .await
                .unwrap();
        }

        let record = app
            .get("/moderation/users/spammer", Some(&moderator))
            .await
            .body;
        assert!(record.contains("Delete all 3 uploads"), "{record}");
        let own = app
            .get("/moderation/users/root", Some(&moderator))
            .await
            .body;
        assert!(!own.contains("/delete-uploads"), "not for higher ranks");

        let url = "/users/spammer/delete-uploads";
        let form = "preset=other&reason=spam&confirm=yes";
        assert_eq!(
            app.post_form(url, Some(&spammer), &[], form).await.status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.post_form("/users/root/delete-uploads", Some(&moderator), &[], form)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        for bad in ["preset=other&reason=spam", "preset=&reason=&confirm=yes"] {
            assert_eq!(
                app.post_form(url, Some(&moderator), &[], bad).await.status,
                StatusCode::BAD_REQUEST,
                "{bad}"
            );
        }
        let started = app.post_form(url, Some(&moderator), &[], form).await;
        assert_eq!(
            started.location.as_deref(),
            Some("/moderation/users/spammer"),
            "{}",
            started.body
        );
        let (total, jobs): (i32, i64) = sqlx::query_as(
            "SELECT (SELECT total FROM post_batches),
                    (SELECT count(*) FROM jobs WHERE kind = 'posts.batch')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((total, jobs), (3, 1));
        // Not twice at once.
        assert_eq!(
            app.post_form(url, Some(&admin), &[], form).await.status,
            StatusCode::BAD_REQUEST
        );
        let record = app
            .get("/moderation/users/spammer", Some(&moderator))
            .await
            .body;
        assert!(record.contains("queued"), "{record}");
        assert!(
            record.contains("started deleting the uploads of"),
            "{record}"
        );
    }
}
