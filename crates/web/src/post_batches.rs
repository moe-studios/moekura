//! Moderating many posts at once, in the background: deleting every
//! upload of a user, from their moderation record, and purging deleted
//! posts, from the purge page.

use axum::Router;
use axum::extract::{Form, Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::jobs::PostBatch as PostBatchJob;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_core::posts::PostStatus;
use moekura_core::search::{Filter, Query as SearchQuery, StatusFilter};
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::post_batches::{self, PostBatch};
use moekura_db::posts::Visibility;
use moekura_db::search::{Count, PageRef, Plan, SearchError};
use moekura_db::users::{self, User};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::moderation::{ReasonForm, check_reason};
use crate::pages::Page;

/// Deleted posts a purge preview shows, to tick.
const PREVIEW: u32 = 60;

/// Most posts ticked for one purge.
pub(crate) const TICKED_MAX: usize = 500;

/// Recent purges listed.
const RECENT: i64 = 20;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users/{name}/delete-uploads", post(delete_uploads_form))
        .route("/moderation/purge", get(purge_page).post(purge_form))
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

/// The deleted-post search `text` for a purge: with `status:deleted`,
/// added if missing, and no other status.
pub(crate) fn purge_search(text: &str) -> Result<SearchQuery, AppError> {
    let invalid = |e: moekura_core::search::SearchError| AppError::Unprocessable(format!("{e}."));
    let query = SearchQuery::parse(text).map_err(invalid)?;
    let other_status = query
        .filters()
        .iter()
        .any(|f| matches!(f, Filter::Status(s) if *s != StatusFilter::Deleted))
        || query
            .conditions
            .iter()
            .any(|c| c.negated && matches!(c.filter, Filter::Status(_)));
    if other_status {
        return Err(AppError::Unprocessable(
            "Only deleted posts can be purged, so leave out other status: searches.".into(),
        ));
    }
    if query.status() == Some(StatusFilter::Deleted) {
        return Ok(query);
    }
    SearchQuery::parse(&format!("{text} status:deleted")).map_err(invalid)
}

/// The deleted posts search `query` finds, as a purge sees them: all of
/// them, whatever the purger's own rating and display settings.
async fn purge_plan(
    state: &AppState,
    current: &CurrentUser,
    query: &SearchQuery,
    per_page: u32,
) -> Result<Plan, AppError> {
    let visibility = Visibility {
        ratings: Vec::new(),
        ..crate::posts::visibility(current)
    };
    if !visibility.statuses.contains(&PostStatus::Deleted) {
        return Err(AppError::Forbidden);
    }
    let config = moekura_core::config::SearchConfig {
        per_page,
        ..state.config.search.clone()
    };
    Plan::resolve(state.db.primary(), query, &visibility, &config)
        .await
        .map_err(search_error)
}

fn search_error(error: SearchError) -> AppError {
    match error {
        SearchError::Invalid(message) => AppError::Unprocessable(message),
        SearchError::Db(e) => e.into(),
    }
}

fn count_number(count: Count) -> i64 {
    match count {
        Count::Exact(n) | Count::About(n) | Count::AtLeast(n) => n,
    }
}

/// Which deleted posts a purge removes.
pub(crate) enum PurgeScope {
    /// Every deleted post a search finds (see [`purge_search`]).
    Search(Box<SearchQuery>),
    /// Posts ticked one by one.
    Ticked(Vec<i64>),
}

/// Starts purging the deleted posts `scope` covers, as `current`;
/// returns the batch.
pub(crate) async fn purge(
    state: &AppState,
    current: &CurrentUser,
    scope: PurgeScope,
) -> Result<PostBatch, AppError> {
    current.require(Permission::PurgePosts)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let db = state.db.primary();
    let (query, ids, total) = match scope {
        PurgeScope::Search(query) => {
            let plan = purge_plan(state, current, &query, PREVIEW).await?;
            let total = count_number(plan.count(db).await.map_err(search_error)?);
            (Some(query.to_string()), None, total)
        }
        PurgeScope::Ticked(ids) => {
            if ids.len() > TICKED_MAX {
                return Err(AppError::BadRequest(format!(
                    "At most {TICKED_MAX} posts at once; purge by search for more"
                )));
            }
            let deleted = post_batches::deleted_among(db, &ids).await?;
            let total = deleted.len() as i64;
            (None, Some(deleted), total)
        }
    };
    if total == 0 {
        return Err(AppError::BadRequest("No deleted posts to purge".into()));
    }
    let mut tx = db.begin().await?;
    let id = post_batches::create_purge(&mut *tx, actor, query.as_deref(), ids.as_deref(), total)
        .await?;
    moekura_db::jobs::enqueue(&mut tx, &PostBatchJob { id }).await?;
    let scope = match (&query, &ids) {
        (Some(query), _) => serde_json::json!({ "batch": id, "query": query, "posts": total }),
        (None, ids) => serde_json::json!({ "batch": id, "ids": ids, "posts": total }),
    };
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::PurgePosts).details(scope),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(id, query, posts = total, "purging deleted posts");
    post_batches::by_id(db, id).await?.ok_or(AppError::NotFound)
}

/// A batch as pages show it.
pub(crate) fn batch_context(b: &PostBatch) -> Value {
    context! {
        id => b.id,
        by => b.creator_name,
        user => b.user_name,
        reason => b.reason,
        query => b.query,
        search_url => b.query.as_deref().map(|q| Value::from_safe_string(crate::templates::search_url(q))),
        ticked => b.post_ids.as_ref().map(Vec::len),
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

#[derive(Debug, Default, Deserialize)]
struct PurgeQuery {
    /// The search to preview; absent before there is one.
    tags: Option<String>,
}

async fn purge_page(page: Page, Query(query): Query<PurgeQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::PurgePosts)?;
    let state = page.state();
    let db = state.db.primary();
    let (preview, error) = match query.tags.as_deref().map(purge_search) {
        None => (None, None),
        Some(Err(AppError::Unprocessable(message))) => (None, Some(message)),
        Some(Err(error)) => return Err(error),
        Some(Ok(search)) => match purge_preview(&page, &search).await {
            Ok(preview) => (Some(preview), None),
            Err(AppError::Unprocessable(message)) => (None, Some(message)),
            Err(error) => return Err(error),
        },
    };
    let recent = post_batches::recent(db, "purge", None, RECENT).await?;
    let status = if error.is_some() {
        axum::http::StatusCode::UNPROCESSABLE_ENTITY
    } else {
        axum::http::StatusCode::OK
    };
    Ok(page.render_with_status(
        status,
        "moderation_purge.html",
        context! {
            tags => query.tags,
            preview => preview,
            error => error,
            ticked_max => TICKED_MAX,
            recent => recent.iter().map(batch_context).collect::<Vec<_>>(),
        },
    ))
}

/// How many deleted posts a search finds, and the first of them to tick.
async fn purge_preview(page: &Page, search: &SearchQuery) -> Result<Value, AppError> {
    let db = page.state().db.primary();
    let plan = purge_plan(page.state(), &page.current, search, PREVIEW).await?;
    let count = plan.count(db).await.map_err(search_error)?;
    let ids = plan
        .ids(db, PageRef::Number(1))
        .await
        .map_err(search_error)?;
    let cards: Vec<Value> = crate::posts::grid(page, db, &ids, None)
        .await?
        .into_iter()
        .map(|(id, card)| context! { id => id, card => card })
        .collect();
    Ok(context! {
        query => search.to_string(),
        search_url => Value::from_safe_string(crate::templates::search_url(&search.to_string())),
        count => crate::posts::count_text(count),
        empty => count_number(count) == 0,
        posts => cards,
    })
}

#[derive(Debug, Default, Deserialize)]
struct PurgeForm {
    #[serde(default)]
    tags: String,
    /// `all` (every match) or `ticked`.
    #[serde(default)]
    scope: String,
    #[serde(default)]
    ids: Vec<i64>,
    /// The box saying the posts go for good.
    #[serde(default)]
    confirm: Option<String>,
}

async fn purge_form(
    page: Page,
    jar: CookieJar,
    axum_extra::extract::Form(form): axum_extra::extract::Form<PurgeForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::PurgePosts)?;
    if form.confirm.is_none() {
        return Err(AppError::BadRequest(
            "Tick the box to confirm the posts are to be purged for good".into(),
        ));
    }
    let scope = match form.scope.as_str() {
        "all" => PurgeScope::Search(Box::new(purge_search(&form.tags)?)),
        "ticked" if form.ids.is_empty() => {
            return Err(AppError::BadRequest("Tick the posts first".into()));
        }
        "ticked" => PurgeScope::Ticked(form.ids),
        _ => return Err(AppError::BadRequest("Purge which posts?".into())),
    };
    purge(page.state(), &page.current, scope).await?;
    Ok((
        flash::set(jar, Flash::Queued),
        Redirect::to("/moderation/purge"),
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

    #[test]
    fn purges_cover_deleted_posts_only() {
        let search = |text: &str| super::purge_search(text).map(|q| q.to_string());
        assert_eq!(search("").unwrap(), "status:deleted");
        assert_eq!(search("user:bob").unwrap(), "user:bob status:deleted");
        assert_eq!(search("status:deleted cat").unwrap(), "cat status:deleted");
        for bad in [
            "status:active",
            "-status:deleted",
            "cat (status:any or dog)",
            "(",
        ] {
            assert!(super::purge_search(bad).is_err(), "{bad}");
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn purging_deleted_posts(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let alice_id = user_id(&pool, "alice").await;
        let mut ids = Vec::new();
        for status in ["deleted", "deleted", "deleted", "active"] {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO posts (rating, status, uploader_id) VALUES ('g', $1, $2) RETURNING id",
            )
            .bind(status)
            .bind(alice_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        assert_eq!(
            app.get("/moderation/purge", Some(&moderator)).await.status,
            StatusCode::FORBIDDEN,
            "moderators can't purge"
        );
        let page = app.get("/moderation/purge", Some(&admin)).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("No purges yet"), "{}", page.body);

        let preview = app
            .get("/moderation/purge?tags=user%3Aalice", Some(&admin))
            .await;
        assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
        assert!(
            preview.body.contains("Purge all 3 posts"),
            "{}",
            preview.body
        );
        let refused = app
            .get("/moderation/purge?tags=status%3Aactive", Some(&admin))
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);

        // Confirmation first; nothing happens without it.
        let unconfirmed = app
            .post_form(
                "/moderation/purge",
                Some(&admin),
                &[],
                "tags=user%3Aalice&scope=all",
            )
            .await;
        assert_eq!(unconfirmed.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            app.post_form(
                "/moderation/purge",
                Some(&alice),
                &[],
                "tags=&scope=all&confirm=yes"
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        // Ticked: only deleted posts are kept.
        let ticked = app
            .post_form(
                "/moderation/purge",
                Some(&admin),
                &[],
                &format!(
                    "tags=user%3Aalice&scope=ticked&ids={}&ids={}&confirm=yes",
                    ids[0], ids[3]
                ),
            )
            .await;
        assert_eq!(
            ticked.location.as_deref(),
            Some("/moderation/purge"),
            "{}",
            ticked.body
        );
        let all = app
            .post_form(
                "/moderation/purge",
                Some(&admin),
                &[],
                "tags=user%3Aalice&scope=all&confirm=yes",
            )
            .await;
        assert_eq!(all.status, StatusCode::SEE_OTHER, "{}", all.body);
        let batches: Vec<(Option<Vec<i64>>, Option<String>, i32)> =
            sqlx::query_as("SELECT post_ids, query, total FROM post_batches ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            batches,
            [
                (Some(vec![ids[0]]), None, 1),
                (None, Some("user:alice status:deleted".to_owned()), 3),
            ]
        );
        let logged: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM mod_actions WHERE action = 'posts.purge_batch'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(logged, 2);
        let page = app.get("/moderation/purge", Some(&admin)).await.body;
        assert!(
            page.contains("1 ticked post") && page.contains("user:alice status:deleted"),
            "{page}"
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_preview_offers_posts_to_tick(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(state, super::routes().merge(crate::upload::routes(max)));
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let id = crate::api::test_support::upload(
            &app,
            &admin,
            &crate::test_support::fixture::png(20, 20),
            "cat",
        )
        .await;
        sqlx::query("UPDATE posts SET status = 'deleted'")
            .execute(&pool)
            .await
            .unwrap();
        let preview = app
            .get("/moderation/purge?tags=cat", Some(&admin))
            .await
            .body;
        assert!(
            preview.contains(&format!("name=\"ids\" value=\"{id}\" form=\"purge\"")),
            "{preview}"
        );
        assert!(
            preview.contains(&format!("href=\"/posts/{id}")),
            "{preview}"
        );
    }
}
