//! Moderation pages.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::jobs::PurgePost;
use moekura_core::moderation::REASON_MAX_LEN;
use moekura_core::moderation::{ActionKind, DisapprovalReason};
use moekura_core::permissions::Permission;
use moekura_core::posts::{PostLock, PostStatus};
use moekura_core::webhooks::Event;
use moekura_db::appeals::{self, AppealError};
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
        .route("/moderation/queue/bulk", post(bulk_review))
        .route("/moderation/flags", get(flag_queue))
        .route("/moderation/appeals", get(appeal_queue))
        .route("/posts/{id}/appeal", post(appeal))
        .route("/posts/{id}/appeals/approve", post(approve_appeal))
        .route("/posts/{id}/appeals/reject", post(reject_appeal))
        .route("/posts/{id}/flag", post(flag))
        .route("/posts/{id}/flags/dismiss", post(dismiss_flags))
        .route("/posts/{id}/approve", post(approve))
        .route("/posts/{id}/disapprove", post(disapprove))
        .route("/posts/{id}/reject", post(reject))
        .route("/posts/{id}/delete", post(delete))
        .route("/posts/{id}/restore", post(restore))
        .route("/posts/{id}/purge", post(purge))
        .route("/posts/{id}/locks", post(set_locks_form))
}

#[derive(Debug, Default, Deserialize)]
struct ReasonForm {
    /// One of the site's preset reasons; `other` or empty for none.
    #[serde(default)]
    preset: String,
    /// Free text, on its own or adding to the preset.
    #[serde(default)]
    reason: String,
}

impl ReasonForm {
    fn reason(&self) -> String {
        let preset = match self.preset.as_str() {
            "other" => "",
            preset => preset,
        };
        moekura_core::moderation::combine_reason(preset, &self.reason)
    }
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
/// Moving a post to deleted settles its open flags as upheld.
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
    if to == PostStatus::Deleted {
        flags::resolve(&mut *tx, id, true, actor).await?;
    }
    if kind == ActionKind::PostApprove {
        posts::set_approver(&mut *tx, id, actor).await?;
    }
    // Restoring a post grants its appeal.
    if from.contains(&PostStatus::Deleted) {
        appeals::resolve(&mut *tx, id, true, actor).await?;
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
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    crate::posts::check_lock(current, &post, PostLock::Status)?;
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
    // The reason is what the post shows in its place.
    if action == PostAction::Delete && reason.is_empty() {
        return Err(AppError::BadRequest(
            "Say why the post is being deleted".into(),
        ));
    }
    change_status(state, current, id, from, to, kind, reason).await?;
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
        &form.reason(),
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
    flag_post(page.state(), &page.current, id, &form.reason()).await?;
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
    let post = posts::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    crate::posts::check_lock(current, &post, PostLock::Status)?;
    state.rate_limits.check_report(user.id).await?;
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

async fn flag_queue(page: Page, Query(query): Query<QueueQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ApprovePosts)?;
    let state = page.state();
    let open = flags::open(state.db.primary(), query.after.unwrap_or(0), FLAG_PAGE).await?;
    // Each post's flags come together, oldest first.
    let mut ids: Vec<i64> = open.iter().map(|f| f.post_id).collect();
    ids.dedup();
    let more = (ids.len() == FLAG_PAGE as usize)
        .then(|| open.iter().find(|f| Some(&f.post_id) == ids.last()))
        .flatten()
        .map(|f| url_value(&format!("/moderation/flags?after={}", f.id)));
    let cards = review_cards(state, &ids, &[]).await?;
    let posts: Vec<Value> = cards
        .into_iter()
        .map(|card| {
            // Cards can be fewer than ids (a post without media), so
            // match flags up by post rather than by position.
            let id = card.get_attr("id").ok().and_then(|v| i64::try_from(v).ok());
            let reasons: Vec<Value> = open
                .iter()
                .filter(|f| Some(f.post_id) == id)
                .map(|f| context! { by => f.creator_name, reason => f.reason })
                .collect();
            context! { ..card, ..context! { flags => reasons } }
        })
        .collect();
    Ok(page.render(
        "moderation_flags.html",
        context! { posts => posts, more_url => more },
    ))
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

#[derive(Debug, Default, Deserialize)]
struct LocksForm {
    /// Present when ticked, one per lock.
    rating: Option<String>,
    tags: Option<String>,
    notes: Option<String>,
    status: Option<String>,
}

async fn set_locks_form(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<LocksForm>,
) -> Result<Response, AppError> {
    let locks: Vec<PostLock> = [
        (form.rating.is_some(), PostLock::Rating),
        (form.tags.is_some(), PostLock::Tags),
        (form.notes.is_some(), PostLock::Notes),
        (form.status.is_some(), PostLock::Status),
    ]
    .into_iter()
    .filter_map(|(ticked, lock)| ticked.then_some(lock))
    .collect();
    set_locks(page.state(), &page.current, id, &locks).await?;
    Ok(back_to(jar, &format!("/posts/{id}")))
}

/// Locks exactly `locks` of post `id`, recording it in the post's history
/// and the log.
pub(crate) async fn set_locks(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    locks: &[PostLock],
) -> Result<(), AppError> {
    current.require(Permission::LockPosts)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    let post = posts::lock(&mut *tx, id).await?.ok_or(AppError::NotFound)?;
    if !crate::posts::visibility(current).allows(&post) {
        return Err(AppError::NotFound);
    }
    let names = |locks: &[PostLock]| -> Vec<&str> {
        PostLock::ALL
            .iter()
            .filter(|l| locks.contains(l))
            .map(|l| l.as_str())
            .collect()
    };
    if names(&post.locks) == names(locks) {
        return Ok(());
    }
    moekura_db::post_versions::attribute(&mut tx, actor, None).await?;
    posts::set_locks(&mut *tx, id, locks).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::PostLock)
            .post(id)
            .details(serde_json::json!({ "from": names(&post.locks), "locks": names(locks) })),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Queues removal of a deleted post and its files.
async fn purge(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    moderate(page.state(), &page.current, id, PostAction::Purge, "").await?;
    Ok(back_to(jar, "/posts?tags=status%3Adeleted"))
}

/// Whether `current` may appeal `post`: a deleted post they can see (their
/// own upload, or any if they see deleted posts), with the right to flag.
fn may_appeal(current: &CurrentUser, post: &posts::Post) -> bool {
    let me = current.user.as_ref().map(|u| u.id);
    post.status == PostStatus::Deleted
        && me.is_some()
        && current.can(Permission::Flag)
        && (post.uploader_id == me || current.can(Permission::ViewDeleted))
}

/// The appeal form and history for a post page: the form for those who
/// may appeal, the history for staff and the uploader.
pub(crate) async fn appeal_context(
    state: &AppState,
    current: &CurrentUser,
    post: &posts::Post,
) -> Result<Value, AppError> {
    let me = current.user.as_ref().map(|u| u.id);
    let staff = current.can(Permission::DeletePosts) || current.can(Permission::ApprovePosts);
    let own = me.is_some() && post.uploader_id == me;
    let history = if staff || own {
        appeals::for_post(state.db.primary(), post.id).await?
    } else {
        Vec::new()
    };
    let open = history.iter().any(|a| a.status == "open");
    Ok(context! {
        can_appeal => may_appeal(current, post) && !open,
        history => history.iter().map(|a| context! {
            by => a.creator_name,
            reason => a.reason,
            status => a.status,
            decided_by => a.resolver_name,
            when => crate::dates::day(a.created_at),
        }).collect::<Vec<_>>(),
    })
}

async fn appeal(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<ReasonForm>,
) -> Result<Response, AppError> {
    appeal_post(page.state(), &page.current, id, &form.reason()).await?;
    Ok(back_to(jar, &format!("/posts/{id}")))
}

/// Appeals deleted post `id`, asking staff to restore it.
pub(crate) async fn appeal_post(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    reason: &str,
) -> Result<(), AppError> {
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = state.db.primary();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    let visible =
        crate::posts::visibility(current).allows(&post) || post.uploader_id == Some(user.id);
    if !visible {
        return Err(AppError::NotFound);
    }
    if !may_appeal(current, &post) {
        return Err(if post.status == PostStatus::Deleted {
            AppError::Forbidden
        } else {
            AppError::BadRequest("Only deleted posts can be appealed".into())
        });
    }
    crate::posts::check_lock(current, &post, PostLock::Status)?;
    let reason = check_reason(reason)?;
    if reason.is_empty() {
        return Err(AppError::BadRequest(
            "Say why the post should come back".into(),
        ));
    }
    state.rate_limits.check_appeal(user.id).await?;
    let mut tx = db.begin().await?;
    match appeals::create(&mut tx, id, user.id, reason).await {
        Ok(_) => {}
        Err(AppealError::Db(e)) => return Err(e.into()),
        Err(e) => return Err(AppError::BadRequest(e.to_string())),
    }
    tx.commit().await?;
    tracing::info!(post_id = id, user = user.name, "post appealed");
    Ok(())
}

/// Appeals per page of their queue.
const APPEAL_PAGE: i64 = 30;

async fn appeal_queue(page: Page, Query(query): Query<QueueQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::DeletePosts)?;
    let state = page.state();
    let db = state.db.primary();
    let open = appeals::open(db, query.after.unwrap_or(0), APPEAL_PAGE).await?;
    let ids: Vec<i64> = open.iter().map(|a| a.post_id).collect();
    let uploaders = uploader_names(db, &ids).await?;
    let cards = review_cards(state, &ids, &uploaders).await?;
    let mut rows = Vec::new();
    for card in cards {
        let Some(id) = card.get_attr("id").ok().and_then(|v| i64::try_from(v).ok()) else {
            continue;
        };
        let Some(appeal) = open.iter().find(|a| a.post_id == id) else {
            continue;
        };
        let deleted = deletion(db, id).await?;
        rows.push(context! {
            ..card,
            ..context! {
                appeal => context! {
                    by => appeal.creator_name,
                    reason => appeal.reason,
                    when => crate::dates::day(appeal.created_at),
                },
                deleted => deleted.map(|e| context! {
                    by => e.actor_name,
                    reason => e.reason,
                    when => crate::dates::day(e.created_at),
                }),
            }
        });
    }
    let more = (open.len() == APPEAL_PAGE as usize)
        .then(|| open.last())
        .flatten()
        .map(|a| url_value(&format!("/moderation/appeals?after={}", a.id)));
    Ok(page.render(
        "moderation_appeals.html",
        context! { posts => rows, more_url => more },
    ))
}

async fn approve_appeal(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    // Restoring grants the appeal.
    moderate(page.state(), &page.current, id, PostAction::Restore, "").await?;
    Ok(back_to(jar, "/moderation/appeals"))
}

async fn reject_appeal(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<ReasonForm>,
) -> Result<Response, AppError> {
    turn_down_appeal(page.state(), &page.current, id, &form.reason()).await?;
    Ok(back_to(jar, "/moderation/appeals"))
}

/// Rejects post `id`'s open appeal, keeping it deleted.
pub(crate) async fn turn_down_appeal(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    reason: &str,
) -> Result<(), AppError> {
    current.require(Permission::DeletePosts)?;
    let reason = check_reason(reason)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    if appeals::resolve(&mut *tx, id, false, actor).await? == 0 {
        return Err(AppError::BadRequest("The post has no open appeal".into()));
    }
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::AppealReject)
            .post(id)
            .reason(reason),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Posts per page of the approval queue.
const QUEUE_PAGE: i64 = 30;

/// Where a moderation queue's page starts.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct QueueQuery {
    pub after: Option<i64>,
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

/// Orders the approval queue offers, as `order:` names and labels.
const QUEUE_ORDERS: [(&str, &str); 7] = [
    ("id_asc", "Oldest first"),
    ("id", "Newest first"),
    ("score", "Highest score"),
    ("favcount", "Most favorites"),
    ("tagcount_asc", "Fewest tags"),
    ("mpixels", "Largest"),
    ("filesize", "Largest file"),
];

/// What the approval queue shows: a search within the pending posts the
/// approver hasn't dealt with, in an order, a page at a time.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ApprovalQuery {
    #[serde(default)]
    tags: String,
    #[serde(default)]
    order: String,
    page: Option<u32>,
}

impl ApprovalQuery {
    /// The queue's URL for `page`.
    fn url(&self, page: u32) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if !self.tags.trim().is_empty() {
            query.append_pair("tags", self.tags.trim());
        }
        if !self.order.is_empty() {
            query.append_pair("order", &self.order);
        }
        if page > 1 {
            query.append_pair("page", &page.to_string());
        }
        let query = query.finish();
        if query.is_empty() {
            "/moderation/queue".to_owned()
        } else {
            format!("/moderation/queue?{query}")
        }
    }

    /// The full search: `status:unmoderated`, the approver's terms and
    /// the order. The error is for the approver.
    fn search(&self) -> Result<moekura_core::search::Query, String> {
        let own = moekura_core::search::Query::parse(&self.tags).map_err(|e| e.to_string())?;
        if own
            .filters()
            .iter()
            .any(|filter| matches!(filter, moekura_core::search::Filter::Status(_)))
        {
            return Err("The queue only has pending posts; leave out status:.".into());
        }
        let order = match (own.order.is_some(), self.order.as_str()) {
            // An order: typed in the box wins.
            (true, _) => String::new(),
            (false, "") => "order:id_asc".to_owned(),
            (false, name) if QUEUE_ORDERS.iter().any(|(o, _)| *o == name) => {
                format!("order:{name}")
            }
            (false, _) => return Err("Unknown order".into()),
        };
        moekura_core::search::Query::parse(&format!(
            "status:unmoderated {} {order}",
            self.tags.trim()
        ))
        .map_err(|e| e.to_string())
    }
}

/// Pending posts matching `query` that the approver hasn't dealt with
/// (`status:unmoderated`).
async fn unmoderated(
    state: &AppState,
    current: &CurrentUser,
    query: &moekura_core::search::Query,
    page: u32,
) -> Result<Result<Vec<i64>, String>, AppError> {
    let mut config = state.config.search.clone();
    config.per_page = QUEUE_PAGE as u32;
    let db = state.db.primary();
    let visibility = crate::posts::visibility(current);
    let page = moekura_db::search::PageRef::Number(page);
    let found = async {
        moekura_db::search::Plan::resolve(db, query, &visibility, &config)
            .await?
            .ids(db, page)
            .await
    }
    .await;
    match found {
        Ok(ids) => Ok(Ok(ids)),
        Err(moekura_db::search::SearchError::Invalid(message)) => Ok(Err(message)),
        Err(moekura_db::search::SearchError::Db(e)) => Err(e.into()),
    }
}

/// Each post's uploader's name, for queues.
async fn uploader_names(
    db: &sqlx::PgPool,
    ids: &[i64],
) -> Result<Vec<(i64, Option<String>)>, AppError> {
    let found = posts::by_ids(db, ids).await?;
    let uploader_ids: Vec<i64> = found.iter().filter_map(|p| p.uploader_id).collect();
    let names = users::names(db, &uploader_ids).await?;
    Ok(found
        .iter()
        .map(|p| {
            let name = p
                .uploader_id
                .and_then(|u| names.iter().find(|(id, _)| *id == u))
                .map(|(_, name)| name.clone());
            (p.id, name)
        })
        .collect())
}

/// The disapprovals of `ids` as the queue and post page show them.
pub(crate) async fn disapprovals(
    db: &sqlx::PgPool,
    ids: &[i64],
) -> Result<Vec<(i64, Value)>, AppError> {
    Ok(moekura_db::disapprovals::for_posts(db, ids)
        .await?
        .into_iter()
        .map(|d| {
            let reason = d
                .reason()
                .map_or(d.reason.clone(), |r| r.label().to_owned());
            (
                d.post_id,
                context! {
                    by => d.user_name,
                    reason => reason,
                    message => d.message,
                    when => crate::dates::day(d.created_at),
                },
            )
        })
        .collect())
}

async fn queue(page: Page, Query(query): Query<ApprovalQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ApprovePosts)?;
    let state = page.state();
    let db = state.db.primary();
    let number = query.page.unwrap_or(1).max(1);
    let found = match query.search() {
        Ok(search) => unmoderated(state, &page.current, &search, number).await?,
        Err(message) => Err(message),
    };
    let (ids, error) = match found {
        Ok(ids) => (ids, None),
        Err(message) => (Vec::new(), Some(message)),
    };
    let uploaders = uploader_names(db, &ids).await?;
    let cards = review_cards(state, &ids, &uploaders).await?;
    let disapprovals = disapprovals(db, &ids).await?;
    let cards: Vec<Value> = cards
        .into_iter()
        .map(|card| {
            let id = card.get_attr("id").ok().and_then(|v| i64::try_from(v).ok());
            let theirs: Vec<&Value> = disapprovals
                .iter()
                .filter(|(post, _)| Some(*post) == id)
                .map(|(_, d)| d)
                .collect();
            context! { ..card, ..context! { disapprovals => theirs } }
        })
        .collect();
    let more = (ids.len() == QUEUE_PAGE as usize).then(|| url_value(&query.url(number + 1)));
    let previous = (number > 1).then(|| url_value(&query.url(number - 1)));
    let status = if error.is_some() {
        axum::http::StatusCode::BAD_REQUEST
    } else {
        axum::http::StatusCode::OK
    };
    Ok(page.render_with_status(
        status,
        "moderation_queue.html",
        context! {
            posts => cards,
            more_url => more,
            previous_url => previous,
            back => query.url(number),
            error => error,
            query => context! {
                tags => query.tags.trim(),
                order => query.order,
            },
            orders => QUEUE_ORDERS
                .iter()
                .map(|(name, label)| context! { name => name, label => label })
                .collect::<Vec<_>>(),
            disapproval_reasons => DisapprovalReason::ALL
                .iter()
                .map(|r| context! { name => r.as_str(), label => r.label() })
                .collect::<Vec<_>>(),
        },
    ))
}

/// Most posts approved or rejected at once.
const BULK_MAX: usize = 100;

#[derive(Debug, Default, Deserialize)]
struct BulkForm {
    #[serde(default)]
    ids: Vec<i64>,
    /// `approve` or `reject`.
    #[serde(default)]
    action: String,
    #[serde(default)]
    preset: String,
    #[serde(default)]
    reason: String,
    /// The queue page to go back to.
    back: Option<String>,
}

/// Approves or rejects the posts ticked in the queue. Posts someone else
/// dealt with meanwhile are skipped.
async fn bulk_review(
    page: Page,
    jar: CookieJar,
    axum_extra::extract::Form(form): axum_extra::extract::Form<BulkForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ApprovePosts)?;
    let action = match form.action.as_str() {
        "approve" => PostAction::Approve,
        "reject" => PostAction::Reject,
        _ => return Err(AppError::BadRequest("Approve or reject?".into())),
    };
    if form.ids.is_empty() {
        return Err(AppError::BadRequest("Tick the posts first".into()));
    }
    if form.ids.len() > BULK_MAX {
        return Err(AppError::BadRequest(format!(
            "At most {BULK_MAX} posts at once"
        )));
    }
    let reason = ReasonForm {
        preset: form.preset.clone(),
        reason: form.reason.clone(),
    }
    .reason();
    check_reason(&reason)?;
    let mut skipped = 0;
    for id in &form.ids {
        match moderate(page.state(), &page.current, *id, action, &reason).await {
            Ok(()) => {}
            // Someone got there first.
            Err(AppError::BadRequest(_) | AppError::NotFound) => skipped += 1,
            Err(error) => return Err(error),
        }
    }
    tracing::info!(
        posts = form.ids.len(),
        skipped,
        action = form.action,
        "posts reviewed in bulk"
    );
    let flash = if skipped > 0 {
        Flash::SomeSkipped
    } else {
        Flash::Saved
    };
    let back = crate::account::safe_next(form.back.as_deref());
    let back = if back.starts_with("/moderation/queue") {
        back
    } else {
        "/moderation/queue"
    };
    Ok((flash::set(jar, flash), Redirect::to(back)).into_response())
}

#[derive(Debug, Deserialize)]
struct DisapproveForm {
    #[serde(default)]
    reason: String,
    #[serde(default)]
    message: String,
}

async fn disapprove(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<DisapproveForm>,
) -> Result<Response, AppError> {
    disapprove_post(page.state(), &page.current, id, &form.reason, &form.message).await?;
    Ok(back_to(jar, "/moderation/queue"))
}

/// Passes on pending post `id` for `current` without rejecting it.
pub(crate) async fn disapprove_post(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    reason: &str,
    message: &str,
) -> Result<(), AppError> {
    current.require(Permission::ApprovePosts)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let reason = DisapprovalReason::parse(reason).ok_or_else(|| {
        AppError::BadRequest("Say why: breaks_rules, poor_quality or disinterest".into())
    })?;
    let message = check_reason(message)?;
    if !moekura_db::disapprovals::disapprove(state.db.primary(), id, user.id, reason, message)
        .await?
    {
        return Err(AppError::BadRequest(
            "Only pending posts can be disapproved".into(),
        ));
    }
    tracing::info!(
        post_id = id,
        user = user.name,
        reason = reason.as_str(),
        "post disapproved"
    );
    Ok(())
}

/// Where approving or rejecting goes back to: the queue, or with
/// `from=post` the post's page.
#[derive(Debug, Default, Deserialize)]
struct ReviewedFrom {
    #[serde(default)]
    from: String,
}

impl ReviewedFrom {
    fn url(&self, id: i64) -> String {
        match self.from.as_str() {
            "post" => format!("/posts/{id}"),
            _ => "/moderation/queue".to_owned(),
        }
    }
}

async fn approve(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Query(from): Query<ReviewedFrom>,
) -> Result<Response, AppError> {
    moderate(page.state(), &page.current, id, PostAction::Approve, "").await?;
    Ok(back_to(jar, &from.url(id)))
}

async fn reject(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Query(from): Query<ReviewedFrom>,
    Form(form): Form<ReasonForm>,
) -> Result<Response, AppError> {
    moderate(
        page.state(),
        &page.current,
        id,
        PostAction::Reject,
        &form.reason(),
    )
    .await?;
    Ok(back_to(jar, &from.url(id)))
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
    /// The user acted on.
    #[serde(default)]
    user: String,
    /// `YYYY-MM-DD`, UTC, inclusive.
    #[serde(default)]
    since: String,
    /// `YYYY-MM-DD`, UTC, inclusive.
    #[serde(default)]
    until: String,
    before: Option<i64>,
}

/// Parses a `YYYY-MM-DD` day; empty is none.
pub(crate) fn parse_day(text: &str) -> Result<Option<time::Date>, AppError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let bad = || AppError::BadRequest(format!("`{text}` isn't a date like 2026-09-28"));
    let mut parts = text.splitn(3, '-');
    let mut next = || {
        parts
            .next()
            .and_then(|p| p.parse::<i32>().ok())
            .ok_or_else(bad)
    };
    let (year, month, day) = (next()?, next()?, next()?);
    let month = u8::try_from(month)
        .ok()
        .and_then(|m| time::Month::try_from(m).ok())
        .ok_or_else(bad)?;
    let day = u8::try_from(day).map_err(|_| bad())?;
    time::Date::from_calendar_date(year, month, day)
        .map(Some)
        .map_err(|_| bad())
}

/// The span of log entries from day `since` through day `until`, UTC.
pub(crate) fn day_span(
    since: Option<time::Date>,
    until: Option<time::Date>,
) -> (Option<time::OffsetDateTime>, Option<time::OffsetDateTime>) {
    (
        since.map(|d| d.midnight().assume_utc()),
        until.map(|d| d.midnight().assume_utc() + time::Duration::days(1)),
    )
}

pub(crate) fn entry_context(entry: &Entry) -> Value {
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
        when => crate::dates::day(entry.created_at),
        time => crate::dates::clock(entry.created_at),
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
    // Nobody by that name: match nothing.
    let user_id = async |name: &str| -> Result<Option<i64>, AppError> {
        match name.trim() {
            "" => Ok(None),
            name => Ok(Some(users::by_name(db, name).await?.map_or(-1, |u| u.id))),
        }
    };
    let (since, until) = day_span(parse_day(&query.since)?, parse_day(&query.until)?);
    let filter = Filter {
        action: ActionKind::parse(&query.action),
        actor_id: user_id(&query.by).await?,
        post_id: query.post.trim().trim_start_matches('#').parse().ok(),
        user_id: user_id(&query.user).await?,
        before: query.before,
        since,
        until,
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
                .append_pair("user", &query.user)
                .append_pair("since", &query.since)
                .append_pair("until", &query.until)
                .append_pair("before", &id.to_string())
                .finish();
            url_value(&format!("/moderation/log?{q}"))
        });
    Ok(page.render(
        "moderation_log.html",
        context! {
            entries => entries.iter().map(entry_context).collect::<Vec<_>>(),
            actions => ActionKind::ALL.iter().map(|k| context! { name => k.as_str(), label => k.label() }).collect::<Vec<_>>(),
            query => context! {
                action => query.action,
                by => query.by,
                post => query.post,
                user => query.user,
                since => query.since,
                until => query.until,
            },
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
            .split('?')
            .next()
            .unwrap()
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
        let unexplained = app
            .post_form(
                &format!("/posts/{id}/delete"),
                Some(&moderator),
                &[],
                "reason=+",
            )
            .await;
        assert_eq!(unexplained.status, StatusCode::BAD_REQUEST);
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
        // Gone for members but the uploader, who sees why; explained to
        // staff.
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        assert_eq!(
            app.get(&format!("/posts/{id}"), Some(&bob)).await.status,
            StatusCode::NOT_FOUND
        );
        let own = app.get(&format!("/posts/{id}"), Some(&alice)).await.body;
        assert!(
            own.contains("“duplicate”") && !own.contains("/restore"),
            "{own}"
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
                    .split('?')
                    .next()
                    .unwrap()
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
        // approver: searches find who let it in.
        let approvers: Vec<Option<i64>> =
            sqlx::query_scalar("SELECT approver_id FROM posts ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            approvers[0].is_some() && approvers[1].is_none(),
            "{approvers:?}"
        );
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
    async fn approvers_pass_on_posts(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::posts::routes()),
        );
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        let kim = session_for(&pool, "kim", SystemRole::Janitor).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let alice_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'alice'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let jan_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'jan'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut ids = Vec::new();
        for (i, uploader) in [alice_id, alice_id, jan_id].into_iter().enumerate() {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO posts (rating, status, uploader_id) VALUES ('g', 'pending', $1) RETURNING id",
            )
            .bind(uploader)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
                 VALUES ($1, sha256($1::text::bytea), substring(sha256($1::text::bytea) FROM 1 FOR 16), 'png', 10, 10, 1, $2)",
            )
            .bind(id)
            .bind(format!("original/aa/aa/{i}.png"))
            .execute(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let queue = app.get("/moderation/queue", Some(&jan)).await.body;
        assert!(
            queue.contains(&format!("/posts/{}/approve", ids[0])),
            "{queue}"
        );
        assert!(
            !queue.contains(&format!("/posts/{}/approve", ids[2])),
            "not their own upload"
        );
        assert_eq!(
            app.post_form(
                &format!("/posts/{}/disapprove", ids[0]),
                Some(&alice),
                &[],
                "reason=poor_quality"
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        let bad = app
            .post_form(
                &format!("/posts/{}/disapprove", ids[0]),
                Some(&jan),
                &[],
                "reason=meh",
            )
            .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        let done = app
            .post_form(
                &format!("/posts/{}/disapprove", ids[0]),
                Some(&jan),
                &[],
                "reason=poor_quality&message=too+blurry",
            )
            .await;
        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.body);
        // Gone from jan's queue; shown to kim, with why.
        let queue = app.get("/moderation/queue", Some(&jan)).await.body;
        assert!(
            !queue.contains(&format!("/posts/{}/approve", ids[0])),
            "{queue}"
        );
        assert!(queue.contains(&format!("/posts/{}/approve", ids[1])));
        let queue = app.get("/moderation/queue", Some(&kim)).await.body;
        assert!(
            queue.contains(&format!("/posts/{}/approve", ids[0])),
            "{queue}"
        );
        assert!(
            queue.contains("by jan: Poor quality — “too blurry”"),
            "{queue}"
        );
        let page = app
            .get(&format!("/posts/{}", ids[0]), Some(&kim))
            .await
            .body;
        assert!(page.contains("too blurry"), "{page}");
        // Still pending, and approvable.
        app.post(&format!("/posts/{}/approve", ids[0]), Some(&kim), &[])
            .await;
        let again = app
            .post_form(
                &format!("/posts/{}/disapprove", ids[0]),
                Some(&kim),
                &[],
                "reason=disinterest",
            )
            .await;
        assert_eq!(again.status, StatusCode::BAD_REQUEST, "no longer pending");
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
                    .split('?')
                    .next()
                    .unwrap()
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
    async fn the_flag_queue_pages(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        sqlx::query(
            "WITH p AS (INSERT INTO posts (rating, status)
                        SELECT 'g', 'flagged' FROM generate_series(1, 31) RETURNING id)
             INSERT INTO post_flags (post_id, reason) SELECT id, 'flag ' || id FROM p",
        )
        .execute(&pool)
        .await
        .unwrap();
        let first = app.get("/moderation/flags", Some(&moderator)).await.body;
        let at = first.find("/moderation/flags?after=").expect("a next page");
        let next: String = first[at..].split('"').next().unwrap().to_owned();
        let second = app.get(&next, Some(&moderator)).await;
        assert_eq!(second.status, StatusCode::OK);
        assert!(
            !second.body.contains("/moderation/flags?after="),
            "last page"
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
        let on_nobody = app
            .get("/moderation/log?user=nobody", Some(&moderator))
            .await;
        assert!(!on_nobody.body.contains("off-topic"));
        let today = time::OffsetDateTime::now_utc().date();
        let span = format!("/moderation/log?since={today}&until={today}",);
        assert!(
            app.get(&span, Some(&moderator))
                .await
                .body
                .contains("off-topic")
        );
        let tomorrow = today.next_day().unwrap();
        let later = app
            .get(
                &format!("/moderation/log?since={tomorrow}"),
                Some(&moderator),
            )
            .await;
        assert!(!later.body.contains("off-topic"));
        let bad = app
            .get("/moderation/log?since=yesterday", Some(&moderator))
            .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn preset_reasons(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::posts::routes()),
        );
        let bob = crate::test_support::session_for(&pool, "bob", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let mut ids = Vec::new();
        for i in 0..3 {
            let id: i64 =
                sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            sqlx::query(
                "INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
                 VALUES ($1, sha256($1::text::bytea), substring(sha256($1::text::bytea) FROM 1 FOR 16), 'png', 10, 10, 1, $2)",
            )
            .bind(id)
            .bind(format!("original/aa/aa/{i}.png"))
            .execute(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let page = app
            .get(&format!("/posts/{}", ids[0]), Some(&moderator))
            .await
            .body;
        assert!(
            page.contains("<option value=\"Duplicate\">Duplicate</option>"),
            "{page}"
        );

        let form = |preset: &str, reason: &str| {
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("preset", preset)
                .append_pair("reason", reason)
                .finish()
        };
        for (id, preset, reason) in [
            (ids[0], "Duplicate", ""),
            (ids[1], "Poor quality", "blurry"),
            (ids[2], "other", "my own words"),
        ] {
            let response = app
                .post_form(
                    &format!("/posts/{id}/delete"),
                    Some(&moderator),
                    &[],
                    &form(preset, reason),
                )
                .await;
            assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        }
        let reasons: Vec<String> =
            sqlx::query_scalar("SELECT reason FROM mod_actions ORDER BY post_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            reasons,
            ["Duplicate", "Poor quality: blurry", "my own words"]
        );
        // "Other" alone says nothing.
        let flagless: i64 =
            sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let unexplained = app
            .post_form(
                &format!("/posts/{flagless}/flag"),
                Some(&bob),
                &[],
                &form("other", ""),
            )
            .await;
        assert_eq!(unexplained.status, StatusCode::BAD_REQUEST);
        let flagged = app
            .post_form(
                &format!("/posts/{flagless}/flag"),
                Some(&bob),
                &[],
                &form("Off-topic", ""),
            )
            .await;
        assert_eq!(flagged.status, StatusCode::SEE_OTHER);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn uploaders_appeal_deleted_posts(pool: PgPool) {
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
            let id: i64 = response.location.unwrap()["/posts/".len()..]
                .split('?')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            app.post_form(
                &format!("/posts/{id}/delete"),
                Some(&moderator),
                &[],
                "reason=off-topic",
            )
            .await;
            ids.push(id);
        }
        let id = ids[0];
        // The uploader sees why, and may appeal; others don't see it.
        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("off-topic"), "{}", page.body);
        assert!(page.body.contains(&format!("/posts/{id}/appeal")));
        assert!(!page.body.contains("id=\"edit-tags\""), "no editing");
        assert_eq!(
            app.get(&format!("/posts/{id}"), Some(&bob)).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.post_form(&format!("/posts/{id}/appeal"), Some(&bob), &[], "reason=x")
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        let unexplained = app
            .post_form(&format!("/posts/{id}/appeal"), Some(&alice), &[], "reason=")
            .await;
        assert_eq!(unexplained.status, StatusCode::BAD_REQUEST);
        for id in &ids {
            let appealed = app
                .post_form(
                    &format!("/posts/{id}/appeal"),
                    Some(&alice),
                    &[],
                    "reason=it+is+on+topic",
                )
                .await;
            assert_eq!(appealed.status, StatusCode::SEE_OTHER, "{}", appealed.body);
        }
        let again = app
            .post_form(
                &format!("/posts/{id}/appeal"),
                Some(&alice),
                &[],
                "reason=x",
            )
            .await;
        assert_eq!(
            again.status,
            StatusCode::BAD_REQUEST,
            "one open appeal at a time"
        );

        // Staff see them in their queue, and in searches.
        assert_eq!(
            app.get("/moderation/appeals", Some(&alice)).await.status,
            StatusCode::FORBIDDEN
        );
        let queue = app.get("/moderation/appeals", Some(&moderator)).await.body;
        assert!(queue.contains("it is on topic"), "{queue}");
        assert!(
            queue.contains(&format!("/posts/{id}/appeals/approve")),
            "{queue}"
        );
        let search = app
            .get("/posts?tags=status%3Aappealed", Some(&moderator))
            .await
            .body;
        assert!(search.contains(&format!("/posts/{id}")), "{search}");

        let restored = app
            .post(
                &format!("/posts/{id}/appeals/approve"),
                Some(&moderator),
                &[],
            )
            .await;
        assert_eq!(restored.status, StatusCode::SEE_OTHER);
        let kept = app
            .post_form(
                &format!("/posts/{}/appeals/reject", ids[1]),
                Some(&moderator),
                &[],
                "reason=still+off-topic",
            )
            .await;
        assert_eq!(kept.status, StatusCode::SEE_OTHER);
        let statuses: Vec<(String, String)> = sqlx::query_as(
            "SELECT p.status, a.status FROM post_appeals a JOIN posts p ON p.id = a.post_id ORDER BY a.id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            statuses,
            [
                ("active".to_owned(), "approved".to_owned()),
                ("deleted".to_owned(), "rejected".to_owned())
            ]
        );
        let logged: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM mod_actions WHERE action LIKE 'appeal.%' OR action = 'post.restore'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(logged, ["post.restore", "appeal.reject"]);
        // The history stays on the post.
        let page = app
            .get(&format!("/posts/{}", ids[1]), Some(&alice))
            .await
            .body;
        assert!(page.contains("rejected"), "{page}");
        assert!(
            page.contains(&format!("/posts/{}/appeal", ids[1])),
            "may appeal again"
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_queue_searches_sorts_and_acts_in_bulk(pool: PgPool) {
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
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        let mut ids = Vec::new();
        for (i, (tags, rating)) in [("cat", "g"), ("dog", "e"), ("cat dog", "g")]
            .iter()
            .enumerate()
        {
            let response = app
                .post_multipart(
                    "/upload",
                    Some(&alice),
                    &[
                        ("rating", (*rating).to_owned()),
                        ("tags", (*tags).to_owned()),
                    ],
                    Some((
                        "a.png",
                        &crate::test_support::fixture::png(20 + 4 * i as u32, 20),
                    )),
                )
                .await;
            ids.push(
                response.location.unwrap()["/posts/".len()..]
                    .split('?')
                    .next()
                    .unwrap()
                    .parse::<i64>()
                    .unwrap(),
            );
        }
        let approve = |id: i64| format!("/posts/{id}/approve");
        let shown = |body: &str| -> Vec<i64> {
            ids.iter()
                .copied()
                .filter(|id| body.contains(&approve(*id)))
                .collect()
        };
        let cats = app.get("/moderation/queue?tags=cat", Some(&jan)).await.body;
        assert_eq!(shown(&cats), [ids[0], ids[2]]);
        let explicit = app
            .get("/moderation/queue?tags=rating%3Ae+user%3Aalice", Some(&jan))
            .await
            .body;
        assert_eq!(shown(&explicit), [ids[1]]);
        // Newest first.
        let newest = app.get("/moderation/queue?order=id", Some(&jan)).await.body;
        let at = |id: i64| newest.find(&approve(id)).unwrap();
        assert!(at(ids[2]) < at(ids[0]), "{newest}");
        let refused = app
            .get("/moderation/queue?tags=status%3Aactive", Some(&jan))
            .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        assert!(
            refused.body.contains("leave out status:"),
            "{}",
            refused.body
        );

        // Approve two at once, then reject the last with a reason.
        let bulk = format!(
            "ids={}&ids={}&action=approve&back=%2Fmoderation%2Fqueue%3Ftags%3Dcat",
            ids[0], ids[2]
        );
        assert_eq!(
            app.post_form("/moderation/queue/bulk", Some(&alice), &[], &bulk)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let done = app
            .post_form("/moderation/queue/bulk", Some(&jan), &[], &bulk)
            .await;
        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.body);
        assert_eq!(done.location.as_deref(), Some("/moderation/queue?tags=cat"));
        // Already approved: skipped, not refused.
        let again = format!(
            "ids={}&ids={}&action=reject&preset=Off-topic",
            ids[0], ids[1]
        );
        let done = app
            .post_form("/moderation/queue/bulk", Some(&jan), &[], &again)
            .await;
        assert_eq!(done.status, StatusCode::SEE_OTHER);
        assert!(done.set_cookie.iter().any(|c| c.contains("some_skipped")));
        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM posts ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(statuses, ["active", "deleted", "active"]);
        let reason: String =
            sqlx::query_scalar("SELECT reason FROM mod_actions WHERE action = 'post.reject'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason, "Off-topic");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn approving_from_the_post_page(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::posts::routes()),
        );
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let mut ids = Vec::new();
        for (i, uploader) in ["jan", "alice"].iter().enumerate() {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO posts (rating, status, uploader_id)
                 SELECT 'g', 'pending', id FROM users WHERE name = $1 RETURNING id",
            )
            .bind(uploader)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
                 VALUES ($1, sha256($1::text::bytea), substring(sha256($1::text::bytea) FROM 1 FOR 16), 'png', 10, 10, 1, $2)",
            )
            .bind(id)
            .bind(format!("original/aa/aa/{i}.png"))
            .execute(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let (own, theirs) = (ids[0], ids[1]);
        // Their own upload isn't in their queue, but its page offers it.
        let queue = app.get("/moderation/queue", Some(&jan)).await.body;
        assert!(!queue.contains(&format!("/posts/{own}/approve")), "{queue}");
        let page = app.get(&format!("/posts/{own}"), Some(&jan)).await.body;
        assert!(
            page.contains(&format!("/posts/{own}/approve?from=post")),
            "{page}"
        );
        let uploader_view = app
            .get(&format!("/posts/{theirs}"), Some(&alice))
            .await
            .body;
        assert!(!uploader_view.contains("/approve"), "{uploader_view}");

        let approved = app
            .post(&format!("/posts/{own}/approve?from=post"), Some(&jan), &[])
            .await;
        assert_eq!(approved.location, Some(format!("/posts/{own}")));
        let rejected = app
            .post_form(
                &format!("/posts/{theirs}/reject?from=post"),
                Some(&jan),
                &[],
                "preset=Duplicate",
            )
            .await;
        assert_eq!(rejected.location, Some(format!("/posts/{theirs}")));
        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM posts ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(statuses, ["active", "deleted"]);
        // Without `from`, back to the queue as before.
        let other: i64 = sqlx::query_scalar(
            "INSERT INTO posts (rating, status) VALUES ('g', 'pending') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let approved = app
            .post(&format!("/posts/{other}/approve"), Some(&jan), &[])
            .await;
        assert_eq!(approved.location.as_deref(), Some("/moderation/queue"));
    }
}
