//! Moderation pages.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::jobs::PurgePost;
use moekura_core::moderation::ActionKind;
use moekura_core::moderation::REASON_MAX_LEN;
use moekura_core::permissions::Permission;
use moekura_core::posts::PostStatus;
use moekura_core::webhooks::Event;
use moekura_db::flags::{self, FlagError};
use moekura_db::mod_actions::NewAction;
use moekura_db::mod_actions::{self, Entry, Filter};
use moekura_db::users;
use moekura_db::{jobs, posts, tags};
use moekura_storage::Key;
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::url_value;

/// Log entries per page.
const LOG_PAGE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/moderation/log", get(log))
        .route("/moderation/queue", get(queue))
        .route("/moderation/flags", get(flag_queue))
        .route("/posts/{id}/flag", post(flag))
        .route("/posts/{id}/flags/dismiss", post(dismiss_flags))
        .route("/posts/{id}/approve", post(approve))
        .route("/posts/{id}/reject", post(reject))
        .route("/posts/{id}/delete", post(delete))
        .route("/posts/{id}/restore", post(restore))
        .route("/posts/{id}/purge", post(purge))
}

#[derive(Debug, Default, Deserialize)]
struct ReasonForm {
    #[serde(default)]
    reason: String,
}

fn check_reason(reason: &str) -> Result<&str, AppError> {
    let reason = reason.trim();
    if reason.chars().count() > REASON_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "The reason may be at most {REASON_MAX_LEN} characters"
        )));
    }
    Ok(reason)
}

/// Moves post `id` between statuses and logs it, in one transaction.
async fn change_status(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    from: &[PostStatus],
    to: PostStatus,
    kind: ActionKind,
    reason: &str,
) -> Result<(), AppError> {
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    if !posts::set_status(&mut *tx, id, from, to).await? {
        return Err(AppError::BadRequest(
            "The post isn't in a state where that applies (someone may have got there first)"
                .into(),
        ));
    }
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, kind).post(id).reason(reason),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(post_id = id, action = kind.as_str(), "post moderated");
    Ok(())
}

fn back_to(jar: CookieJar, url: &str) -> Response {
    (flash::set(jar, Flash::Saved), Redirect::to(url)).into_response()
}

/// What a moderator can do to a post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostAction {
    /// Let a pending post in.
    Approve,
    /// Turn a pending post away (it's deleted).
    Reject,
    Delete,
    Restore,
    /// Remove a deleted post and its files for good, in the background.
    Purge,
}

/// Applies `action` to post `id` with `current`'s permissions, and logs
/// it. `reason` is kept for rejections and deletions.
pub(crate) async fn moderate(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    action: PostAction,
    reason: &str,
) -> Result<(), AppError> {
    let db = state.db.primary();
    let actor = current.user.as_ref().map(|u| u.id);
    let (permission, from, to, kind, reason): (_, &[PostStatus], _, _, _) = match action {
        PostAction::Approve => (
            Permission::ApprovePosts,
            &[PostStatus::Pending],
            PostStatus::Active,
            ActionKind::PostApprove,
            "",
        ),
        PostAction::Reject => (
            Permission::ApprovePosts,
            &[PostStatus::Pending],
            PostStatus::Deleted,
            ActionKind::PostReject,
            reason,
        ),
        PostAction::Delete => (
            Permission::DeletePosts,
            &[PostStatus::Active, PostStatus::Flagged, PostStatus::Pending],
            PostStatus::Deleted,
            ActionKind::PostDelete,
            reason,
        ),
        PostAction::Restore => (
            Permission::DeletePosts,
            &[PostStatus::Deleted],
            PostStatus::Active,
            ActionKind::PostRestore,
            "",
        ),
        PostAction::Purge => {
            current.require(Permission::PurgePosts)?;
            let mut tx = db.begin().await?;
            let post = posts::lock(&mut *tx, id).await?.ok_or(AppError::NotFound)?;
            if post.status != PostStatus::Deleted {
                return Err(AppError::BadRequest(
                    "Only deleted posts can be purged".into(),
                ));
            }
            mod_actions::record(
                &mut *tx,
                NewAction::new(actor, ActionKind::PostPurge).post(id),
            )
            .await?;
            jobs::enqueue(&mut tx, &PurgePost { post_id: id }).await?;
            tx.commit().await?;
            return Ok(());
        }
    };
    current.require(permission)?;
    let reason = check_reason(reason)?;
    change_status(state, current, id, from, to, kind, reason).await?;
    if action == PostAction::Delete {
        // Deleting settles any open flags.
        flags::resolve(db, id, true, actor).await?;
    }
    let event = match action {
        PostAction::Approve => Some(Event::PostApproved),
        PostAction::Reject | PostAction::Delete => Some(Event::PostDeleted),
        _ => None,
    };
    if let Some(event) = event {
        crate::webhooks::emit_post(state, event, id, serde_json::json!({ "reason": reason })).await;
    }
    Ok(())
}

async fn delete(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<ReasonForm>,
) -> Result<Response, AppError> {
    moderate(
        page.state(),
        &page.current,
        id,
        PostAction::Delete,
        &form.reason,
    )
    .await?;
    Ok(back_to(jar, &format!("/posts/{id}")))
}

async fn flag(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<ReasonForm>,
) -> Result<Response, AppError> {
    flag_post(page.state(), &page.current, id, &form.reason).await?;
    Ok(back_to(jar, &format!("/posts/{id}")))
}

/// Flags post `id` for deletion.
pub(crate) async fn flag_post(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    reason: &str,
) -> Result<(), AppError> {
    current.require(Permission::Flag)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let reason = check_reason(reason)?;
    if reason.is_empty() {
        return Err(AppError::BadRequest("Say why the post should go".into()));
    }
    let mut tx = state.db.primary().begin().await?;
    match flags::create(&mut tx, id, user.id, reason).await {
        Ok(()) => {}
        Err(FlagError::Db(e)) => return Err(e.into()),
        Err(e) => return Err(AppError::BadRequest(e.to_string())),
    }
    tx.commit().await?;
    tracing::info!(post_id = id, user = user.name, "post flagged");
    crate::webhooks::emit_post(
        state,
        Event::PostFlagged,
        id,
        serde_json::json!({ "reason": reason }),
    )
    .await;
    Ok(())
}

/// Posts with open flags per page of the flag queue.
const FLAG_PAGE: i64 = 30;

async fn flag_queue(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ApprovePosts)?;
    let state = page.state();
    let open = flags::open(state.db.primary(), FLAG_PAGE).await?;
    let mut ids: Vec<i64> = open.iter().map(|f| f.post_id).collect();
    ids.dedup();
    let cards = review_cards(state, &ids, &[]).await?;
    let posts: Vec<Value> = cards
        .into_iter()
        .zip(&ids)
        .map(|(card, id)| {
            let reasons: Vec<Value> = open
                .iter()
                .filter(|f| f.post_id == *id)
                .map(|f| context! { by => f.creator_name, reason => f.reason })
                .collect();
            context! { ..card, ..context! { flags => reasons } }
        })
        .collect();
    Ok(page.render("moderation_flags.html", context! { posts => posts }))
}

async fn dismiss_flags(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    dismiss(page.state(), &page.current, id).await?;
    Ok(back_to(jar, "/moderation/flags"))
}

/// Dismisses post `id`'s open flags, keeping the post. Returns how many
/// there were.
pub(crate) async fn dismiss(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<u64, AppError> {
    current.require(Permission::ApprovePosts)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    let dismissed = flags::resolve(&mut *tx, id, false, actor).await?;
    if dismissed == 0 {
        return Err(AppError::BadRequest("The post has no open flags".into()));
    }
    posts::set_status(&mut *tx, id, &[PostStatus::Flagged], PostStatus::Active).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::FlagDismiss)
            .post(id)
            .details(serde_json::json!({ "flags": dismissed })),
    )
    .await?;
    tx.commit().await?;
    Ok(dismissed)
}

async fn restore(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    moderate(page.state(), &page.current, id, PostAction::Restore, "").await?;
    Ok(back_to(jar, &format!("/posts/{id}")))
}

/// Queues removal of a deleted post and its files.
async fn purge(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    moderate(page.state(), &page.current, id, PostAction::Purge, "").await?;
    Ok(back_to(jar, "/posts?tags=status%3Adeleted"))
}

/// Posts per page of the approval queue.
const QUEUE_PAGE: i64 = 30;

#[derive(Debug, Default, Deserialize)]
struct QueueQuery {
    after: Option<i64>,
}

/// Grid-like cards for `ids` with their tag names, for queues.
pub(crate) async fn review_cards(
    state: &AppState,
    ids: &[i64],
    uploaders: &[(i64, Option<String>)],
) -> Result<Vec<Value>, AppError> {
    let db = state.db.primary();
    let sizes = &state.media.config().thumbnail_sizes;
    let size = sizes.get(1).or(sizes.first()).copied().unwrap_or(250);
    let kind = format!("thumb-{size}");
    let cards = posts::cards(db, ids, (&kind, &kind)).await?;
    let mut tag_ids: Vec<i32> = cards
        .iter()
        .flat_map(|c| c.tag_ids.iter().copied())
        .collect();
    tag_ids.sort_unstable();
    tag_ids.dedup();
    let names: std::collections::HashMap<i32, String> = tags::by_ids(db, &tag_ids)
        .await?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect();
    Ok(cards
        .iter()
        .map(|card| {
            let mut card_tags: Vec<&str> = card
                .tag_ids
                .iter()
                .filter_map(|id| names.get(id).map(String::as_str))
                .collect();
            card_tags.sort_unstable();
            let thumb = card
                .thumb
                .as_deref()
                .and_then(Key::parse)
                .map(|k| url_value(&state.file_url(&k)));
            context! {
                id => card.id,
                thumb => thumb,
                rating => card.rating,
                tags => card_tags.join(" "),
                uploader => uploaders.iter().find(|(id, _)| *id == card.id).and_then(|(_, u)| u.clone()),
            }
        })
        .collect())
}

async fn queue(page: Page, Query(query): Query<QueueQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ApprovePosts)?;
    let state = page.state();
    let pending = posts::by_status(
        state.db.primary(),
        PostStatus::Pending,
        query.after.unwrap_or(0),
        QUEUE_PAGE,
    )
    .await?;
    let ids: Vec<i64> = pending.iter().map(|(id, _)| *id).collect();
    let cards = review_cards(state, &ids, &pending).await?;
    let more = (ids.len() == QUEUE_PAGE as usize)
        .then(|| {
            ids.last()
                .map(|id| url_value(&format!("/moderation/queue?after={id}")))
        })
        .flatten();
    Ok(page.render(
        "moderation_queue.html",
        context! { posts => cards, more_url => more },
    ))
}

async fn approve(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    moderate(page.state(), &page.current, id, PostAction::Approve, "").await?;
    Ok(back_to(jar, "/moderation/queue"))
}

async fn reject(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<ReasonForm>,
) -> Result<Response, AppError> {
    moderate(
        page.state(),
        &page.current,
        id,
        PostAction::Reject,
        &form.reason,
    )
    .await?;
    Ok(back_to(jar, "/moderation/queue"))
}

/// The latest deletion or rejection of a post, for its page.
pub(crate) async fn deletion(db: &sqlx::PgPool, post_id: i64) -> Result<Option<Entry>, AppError> {
    let filter = Filter {
        post_id: Some(post_id),
        ..Filter::default()
    };
    Ok(mod_actions::list(db, &filter, 50)
        .await?
        .into_iter()
        .find(|e| e.action == "post.delete" || e.action == "post.reject"))
}

#[derive(Debug, Default, Deserialize)]
struct LogQuery {
    #[serde(default)]
    action: String,
    /// Moderator name.
    #[serde(default)]
    by: String,
    #[serde(default)]
    post: String,
    before: Option<i64>,
}

fn entry_context(entry: &Entry) -> Value {
    let kind = ActionKind::parse(&entry.action);
    let details: Vec<Value> = entry
        .details
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    let text = match value {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    context! { key => key, value => text }
                })
                .collect()
        })
        .unwrap_or_default();
    context! {
        id => entry.id,
        when => entry.created_at.date().to_string(),
        time => format!("{:02}:{:02}", entry.created_at.hour(), entry.created_at.minute()),
        actor => entry.actor_name,
        label => kind.map_or_else(|| entry.action.clone(), |k| k.label().to_owned()),
        post_id => entry.post_id,
        user => entry.user_name,
        reason => entry.reason,
        details => details,
    }
}

async fn log(page: Page, Query(query): Query<LogQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewAuditLog)?;
    let db = page.state().reader(&page.current);
    let actor_id = match query.by.trim() {
        "" => None,
        name => Some(users::by_name(db, name).await?.map_or(-1, |user| user.id)),
    };
    let filter = Filter {
        action: ActionKind::parse(&query.action),
        actor_id,
        post_id: query.post.trim().trim_start_matches('#').parse().ok(),
        user_id: None,
        before: query.before,
    };
    let entries = mod_actions::list(db, &filter, LOG_PAGE).await?;
    let older = (entries.len() == LOG_PAGE as usize)
        .then(|| entries.last().map(|e| e.id))
        .flatten()
        .map(|id| {
            let q = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("action", &query.action)
                .append_pair("by", &query.by)
                .append_pair("post", &query.post)
                .append_pair("before", &id.to_string())
                .finish();
            url_value(&format!("/moderation/log?{q}"))
        });
    Ok(page.render(
        "moderation_log.html",
        context! {
            entries => entries.iter().map(entry_context).collect::<Vec<_>>(),
            actions => ActionKind::ALL.iter().map(|k| context! { name => k.as_str(), label => k.label() }).collect::<Vec<_>>(),
            query => context! { action => query.action, by => query.by, post => query.post },
            older_url => older,
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::moderation::ActionKind;
    use moekura_core::permissions::SystemRole;
    use moekura_db::mod_actions::{self, NewAction};
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn delete_restore_and_purge(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            super::routes()
                .merge(crate::posts::routes())
                .merge(crate::upload::routes(max)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let response = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &[("rating", "g".to_owned()), ("tags", "cat".to_owned())],
                Some(("a.png", &crate::test_support::fixture::png(20, 20))),
            )
            .await;
        let id: i64 = response.location.unwrap()["/posts/".len()..]
            .parse()
            .unwrap();
        let count = || async {
            moekura_db::tags::by_name(&pool, "cat")
                .await
                .unwrap()
                .unwrap()
                .post_count
        };

        assert_eq!(
            app.post_form(
                &format!("/posts/{id}/delete"),
                Some(&alice),
                &[],
                "reason=x"
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        let response = app
            .post_form(
                &format!("/posts/{id}/delete"),
                Some(&moderator),
                &[],
                "reason=duplicate",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        assert_eq!(count().await, 0);
        // Gone for members; explained to staff.
        assert_eq!(
            app.get(&format!("/posts/{id}"), Some(&alice)).await.status,
            StatusCode::NOT_FOUND
        );
        let page = app
            .get(&format!("/posts/{id}"), Some(&moderator))
            .await
            .body;
        assert!(page.contains("by mod: “duplicate”"), "{page}");
        assert!(page.contains("/restore"));
        assert!(!page.contains("/purge"), "moderators can't purge");

        app.post(&format!("/posts/{id}/restore"), Some(&moderator), &[])
            .await;
        assert_eq!(count().await, 1);
        // Purging needs a deleted post.
        let refused = app
            .post(&format!("/posts/{id}/purge"), Some(&admin), &[])
            .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        app.post_form(
            &format!("/posts/{id}/delete"),
            Some(&moderator),
            &[],
            "reason=again",
        )
        .await;
        let response = app
            .post(&format!("/posts/{id}/purge"), Some(&admin), &[])
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        let job: String = sqlx::query_scalar("SELECT kind FROM jobs WHERE kind = 'posts.purge'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(job, "posts.purge");
        let logged: Vec<String> =
            sqlx::query_scalar("SELECT action FROM mod_actions WHERE post_id = $1 ORDER BY id")
                .bind(id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            logged,
            ["post.delete", "post.restore", "post.delete", "post.purge"]
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_approval_queue(pool: PgPool) {
        moekura_db::settings::set(&pool, "upload_approval", serde_json::json!(true))
            .await
            .unwrap();
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            super::routes()
                .merge(crate::posts::routes())
                .merge(crate::upload::routes(max)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let janitor = session_for(&pool, "jan", SystemRole::Janitor).await;
        let mut ids = Vec::new();
        for (i, tags) in ["cat", "dog"].iter().enumerate() {
            let response = app
                .post_multipart(
                    "/upload",
                    Some(&alice),
                    &[("rating", "g".to_owned()), ("tags", (*tags).to_owned())],
                    Some((
                        "a.png",
                        &crate::test_support::fixture::png(20 + 4 * i as u32, 20),
                    )),
                )
                .await;
            ids.push(
                response.location.unwrap()["/posts/".len()..]
                    .parse::<i64>()
                    .unwrap(),
            );
        }
        assert_eq!(
            app.get("/moderation/queue", Some(&alice)).await.status,
            StatusCode::FORBIDDEN
        );
        let queue = app.get("/moderation/queue", Some(&janitor)).await.body;
        let first = queue.find(&format!("/posts/{}/approve", ids[0])).unwrap();
        let second = queue.find(&format!("/posts/{}/approve", ids[1])).unwrap();
        assert!(first < second, "oldest first");
        assert!(queue.contains("class=\"review-tags\">cat<"), "{queue}");

        app.post(&format!("/posts/{}/approve", ids[0]), Some(&janitor), &[])
            .await;
        app.post_form(
            &format!("/posts/{}/reject", ids[1]),
            Some(&janitor),
            &[],
            "reason=blurry",
        )
        .await;
        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM posts ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(statuses, ["active", "deleted"]);
        assert!(
            !app.get("/moderation/queue", Some(&janitor))
                .await
                .body
                .contains("/approve")
        );
        // Approving twice is refused.
        let again = app
            .post(&format!("/posts/{}/approve", ids[0]), Some(&janitor), &[])
            .await;
        assert_eq!(again.status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn flags_are_raised_and_settled(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            super::routes()
                .merge(crate::posts::routes())
                .merge(crate::upload::routes(max)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let mut ids = Vec::new();
        for i in 0..2u32 {
            let response = app
                .post_multipart(
                    "/upload",
                    Some(&alice),
                    &[("rating", "g".to_owned())],
                    Some(("a.png", &crate::test_support::fixture::png(20 + 4 * i, 20))),
                )
                .await;
            ids.push(
                response.location.unwrap()["/posts/".len()..]
                    .parse::<i64>()
                    .unwrap(),
            );
        }
        let status = |id: i64| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, String>("SELECT status FROM posts WHERE id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };

        let page = app
            .get(&format!("/posts/{}", ids[0]), Some(&bob))
            .await
            .body;
        assert!(page.contains(&format!("/posts/{}/flag", ids[0])), "{page}");
        for id in &ids {
            let response = app
                .post_form(
                    &format!("/posts/{id}/flag"),
                    Some(&bob),
                    &[],
                    "reason=off-topic",
                )
                .await;
            assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        }
        let again = app
            .post_form(
                &format!("/posts/{}/flag", ids[0]),
                Some(&bob),
                &[],
                "reason=x",
            )
            .await;
        assert_eq!(again.status, StatusCode::BAD_REQUEST);
        assert_eq!(status(ids[0]).await, "flagged");

        let queue = app.get("/moderation/flags", Some(&moderator)).await.body;
        assert!(queue.contains("off-topic"), "{queue}");
        app.post(
            &format!("/posts/{}/flags/dismiss", ids[0]),
            Some(&moderator),
            &[],
        )
        .await;
        assert_eq!(status(ids[0]).await, "active");
        app.post_form(
            &format!("/posts/{}/delete", ids[1]),
            Some(&moderator),
            &[],
            "reason=agreed",
        )
        .await;
        assert_eq!(status(ids[1]).await, "deleted");
        let flag_states: Vec<String> =
            sqlx::query_scalar("SELECT status FROM post_flags ORDER BY post_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(flag_states, ["dismissed", "upheld"]);
        assert!(
            !app.get("/moderation/flags", Some(&moderator))
                .await
                .body
                .contains("off-topic")
        );
        // Staff see a post's flag history.
        let page = app
            .get(&format!("/posts/{}", ids[0]), Some(&moderator))
            .await
            .body;
        assert!(
            page.contains("off-topic") && page.contains("dismissed"),
            "{page}"
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_log_is_for_moderators(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let member = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        mod_actions::record(
            &pool,
            NewAction::new(None, ActionKind::PostDelete)
                .post(12)
                .reason("off-topic")
                .details(serde_json::json!({ "tags": "a b" })),
        )
        .await
        .unwrap();

        assert_eq!(
            app.get("/moderation/log", Some(&member)).await.status,
            StatusCode::FORBIDDEN
        );
        let page = app.get("/moderation/log", Some(&moderator)).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("deleted post"), "{}", page.body);
        assert!(page.body.contains("href=\"/posts/12\""));
        assert!(page.body.contains("off-topic"));
        let filtered = app
            .get("/moderation/log?action=user.ban", Some(&moderator))
            .await;
        assert!(!filtered.body.contains("off-topic"));
        let by_nobody = app.get("/moderation/log?by=nobody", Some(&moderator)).await;
        assert!(!by_nobody.body.contains("off-topic"));
    }
}
