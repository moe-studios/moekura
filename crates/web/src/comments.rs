//! Comments: the thread under each post, posting, editing and deleting
//! your own, votes, reports and hiding by staff, and the list of recent
//! comments.

use std::net::IpAddr;

use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::http::header::ACCEPT;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::{ActionKind, REASON_MAX_LEN};
use moekura_core::permissions::Permission;
use moekura_core::posts::PostStatus;
use moekura_db::comments::{self, Comment, Filter, ReportError};
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::posts::{self, Post};
use moekura_db::users;
use serde::{Deserialize, Serialize};

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

/// Comments scored this low or lower are collapsed.
const COLLAPSE_AT: i32 = -5;

/// Reported comments per page of the queue.
const REPORT_PAGE: i64 = 30;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/comments", get(index))
        .route("/comments/{id}", get(show))
        .route("/comments/{id}/edit", get(edit_form).post(edit))
        .route("/comments/{id}/delete", post(delete))
        .route("/comments/{id}/vote", post(vote))
        .route("/comments/{id}/report", post(report))
        .route("/comments/{id}/hide", post(hide))
        .route("/comments/{id}/restore", post(restore))
        .route("/comments/{id}/sticky", post(sticky))
        .route("/comments/{id}/reports/dismiss", post(dismiss_reports))
        .route("/moderation/comments", get(report_queue))
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
pub(crate) fn sees_deleted(current: &CurrentUser) -> bool {
    current.can(Permission::ViewDeleted) || current.can(Permission::ModerateComments)
}

/// Comment `id` and its post, if `current` may see them.
pub(crate) async fn visible_comment(
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
/// `captcha` is the answer to the captcha new accounts are asked for,
/// sent from `ip`; `None` where no widget was shown (API clients that
/// don't pass a token on).
pub(crate) async fn commenter(
    state: &AppState,
    current: &CurrentUser,
    post: &Post,
    captcha: Option<&str>,
    ip: Option<IpAddr>,
) -> Result<i64, AppError> {
    current.require(Permission::Comment)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    if post.status == PostStatus::Deleted {
        return Err(AppError::Unprocessable(
            "Deleted posts can't be commented on.".into(),
        ));
    }
    state.rate_limits.check_comment(user.id).await?;
    check_captcha(state, current, captcha, ip).await?;
    Ok(user.id)
}

/// Checks the captcha `current` has to solve to comment while their
/// account is new, if one is asked for: every way of commenting goes
/// through here.
async fn check_captcha(
    state: &AppState,
    current: &CurrentUser,
    token: Option<&str>,
    ip: Option<IpAddr>,
) -> Result<(), AppError> {
    let Some(captcha) = crate::captcha::for_comment(state, current) else {
        return Ok(());
    };
    let token = token.ok_or_else(|| {
        AppError::Unprocessable(
            "New accounts have to solve a captcha to comment: comment on the site, or send \
             a solved captcha's token as `captcha`."
                .into(),
        )
    })?;
    captcha.check(token, ip).await.map_err(AppError::BadRequest)
}

/// Posts `current`'s comment (already checked by [`commenter`]) as user
/// `user`, unless the spam filter holds it; returns its id and whether it
/// was held.
pub(crate) async fn publish(
    state: &AppState,
    current: &CurrentUser,
    post_id: i64,
    user: i64,
    body: &str,
    bump: bool,
) -> Result<(i64, bool), AppError> {
    let held = crate::held::check(state, current, body).await?;
    let id = comments::create_held(
        state.db.primary(),
        post_id,
        user,
        body,
        bump,
        held.as_deref(),
    )
    .await?;
    tracing::info!(
        post = post_id,
        comment = id,
        held = held.is_some(),
        "comment posted"
    );
    if held.is_none() {
        published(state, Some(user), id, post_id, body).await;
    }
    Ok((id, held.is_some()))
}

/// Tells webhooks and those mentioned or quoted of a new comment.
pub(crate) async fn published(
    state: &AppState,
    creator: Option<i64>,
    id: i64,
    post_id: i64,
    body: &str,
) {
    crate::webhooks::emit_comment(state, id).await;
    crate::notifications::comment_posted(state, creator, id, post_id, body).await;
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

/// Saves `body` (already cleaned) as the new text of `comment`, which
/// [`check_author`] let `current` change. Like a new comment, a changed
/// text counts against the rate limit and goes through the spam filter,
/// which can hold the comment again; returns whether it did.
pub(crate) async fn save_edit(
    state: &AppState,
    current: &CurrentUser,
    comment: &Comment,
    body: &str,
) -> Result<bool, AppError> {
    if body == comment.body {
        return Ok(false);
    }
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    state.rate_limits.check_comment(user.id).await?;
    let held = crate::held::check(state, current, body).await?;
    comments::update_held(state.db.primary(), comment.id, body, held.as_deref()).await?;
    if held.is_some() {
        tracing::info!(comment = comment.id, "comment edit held");
    }
    Ok(held.is_some())
}

/// Comments for templates, with the viewer's votes.
async fn comment_contexts(
    state: &AppState,
    current: &CurrentUser,
    comments: &[Comment],
) -> Result<Vec<Value>, AppError> {
    let votes = match &current.user {
        Some(user) => {
            let ids: Vec<i64> = comments.iter().map(|c| c.id).collect();
            comments::votes_of(state.db.primary(), user.id, &ids).await?
        }
        None => Vec::new(),
    };
    Ok(comments
        .iter()
        .map(|c| {
            let vote = votes
                .iter()
                .find(|(id, _)| *id == c.id)
                .map_or(0, |(_, v)| *v);
            comment_context(current, c, vote)
        })
        .collect())
}

/// A comment for templates; `vote` is the viewer's.
fn comment_context(current: &CurrentUser, comment: &Comment, vote: i16) -> Value {
    let me = current.user.as_ref().map(|u| u.id);
    let own = comment.creator_id.is_some() && comment.creator_id == me;
    let may_comment = current.can(Permission::Comment);
    let moderate = current.can(Permission::ModerateComments);
    context! {
        id => comment.id,
        post_id => comment.post_id,
        url => url_value(&url(comment)),
        author => comment.creator_name,
        author_url => comment.creator_name.as_deref().map(|name| {
            url_value(&format!("/users/{}", url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>()))
        }),
        html => Value::from_safe_string(markup::render(&comment.body)),
        date => crate::dates::day(comment.created_at),
        created_iso => comment.created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        edited => comment.edited_at.is_some(),
        deleted => comment.is_deleted,
        score => comment.score,
        vote => vote,
        collapsed => comment.score <= COLLAPSE_AT,
        can_vote => current.is_logged_in() && current.can(Permission::Vote) && !own && !comment.is_deleted,
        can_edit => own && may_comment && !comment.is_deleted,
        can_reply => may_comment && current.is_logged_in() && !comment.is_deleted,
        can_report => current.is_logged_in() && current.can(Permission::Flag) && !own && !comment.is_deleted,
        sticky => comment.is_sticky,
        can_stick => moderate && !comment.is_deleted,
        can_hide => moderate && !comment.is_deleted,
        can_restore => moderate && comment.is_deleted,
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
    let hidden = current.user.as_ref().is_some_and(|u| {
        moekura_core::user_settings::UserSettings::from_json(&u.settings).hide_comments
    });
    // A refused comment is shown again even so.
    if hidden && draft.is_none() {
        return Ok(context! {
            hidden => true,
            count => post.comment_count,
            all_url => url_value(&format!("/comments?post_id={}", post.id)),
        });
    }
    let db = state.db.primary();
    let with_deleted = sees_deleted(current);
    let shown = comments::for_post(db, post.id, with_deleted, THREAD_SIZE).await?;
    let visible_shown = shown.iter().filter(|c| !c.is_deleted).count() as i64;
    let older = i64::from(post.comment_count) - visible_shown;
    let list = comment_contexts(state, current, &shown).await?;
    let can_comment = current.is_logged_in()
        && current.can(Permission::Comment)
        && post.status != PostStatus::Deleted;
    Ok(context! {
        comments => list,
        count => post.comment_count,
        older => (older > 0).then_some(older),
        all_url => url_value(&format!("/comments?post_id={}", post.id)),
        can_comment => can_comment,
        captcha => can_comment
            .then(|| crate::captcha::for_comment(state, current).map(|c| c.widget()))
            .flatten(),
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

/// What to tell someone whose comment or edit was refused, beside the
/// text they wrote.
fn refusal(error: &AppError) -> String {
    match error {
        AppError::TooManyRequests { retry_after_secs } => {
            format!("You're commenting too quickly. Try again in {retry_after_secs} seconds.")
        }
        other => other.public_message().to_owned(),
    }
}

#[derive(Debug, Deserialize)]
struct CommentForm {
    body: String,
    /// Present to leave the post's place in `order:comment_bumped` alone.
    do_not_bump: Option<String>,
    /// The captcha widget's token, for new accounts when asked for.
    #[serde(default, alias = "cf-turnstile-response", alias = "h-captcha-response")]
    captcha: String,
}

async fn create(
    page: Page,
    info: crate::auth::RequestInfo,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<CommentForm>,
) -> Result<Response, AppError> {
    let state = page.state();
    let post = visible_post(state, &page.current, id).await?;
    let refused = |error: AppError| CommentDraft {
        body: form.body.clone(),
        error: Some(refusal(&error)),
    };
    let result = async {
        let body = clean_body(&form.body)?;
        let user = commenter(state, &page.current, &post, Some(&form.captcha), info.ip).await?;
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
    let bump = form.do_not_bump.is_none();
    let (comment_id, held) = publish(state, &page.current, post.id, user, &body, bump).await?;
    if held {
        return Ok((
            flash::set(jar, Flash::Held),
            Redirect::to(&format!("/posts/{}#comments", post.id)),
        )
            .into_response());
    }
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
    let saved = async {
        let body = clean_body(&form.body)?;
        save_edit(page.state(), &page.current, &comment, &body).await
    }
    .await;
    match saved {
        Ok(true) => Ok((
            flash::set(jar, Flash::Held),
            Redirect::to(&format!("/posts/{}#comments", comment.post_id)),
        )
            .into_response()),
        Ok(false) => {
            Ok((flash::set(jar, Flash::Saved), Redirect::to(&url(&comment))).into_response())
        }
        Err(
            error @ (AppError::Unauthorized
            | AppError::Forbidden
            | AppError::Blocked(_)
            | AppError::Internal(_)),
        ) => Err(error),
        Err(error) => Ok(page.render_with_status(
            error.status(),
            "comment_edit.html",
            edit_context(&comment, &form.body, Some(refusal(&error))),
        )),
    }
}

async fn delete(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let (comment, _) = visible_comment(page.state(), &page.current, id).await?;
    check_author(&page.current, &comment)?;
    comments::set_deleted(page.state().db.primary(), id, true).await?;
    tracing::info!(comment = id, "comment deleted by its author");
    Ok(Redirect::to(&format!("/posts/{}#comments", comment.post_id)).into_response())
}

#[derive(Debug, Deserialize)]
struct VoteForm {
    /// `1`, `-1`, or `0` to take the vote back.
    score: i16,
}

/// A comment's score and the viewer's vote.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub(crate) struct CommentScore {
    pub score: i32,
    /// Your vote: 1, -1, or 0 for none.
    pub vote: i16,
}

/// Records `current`'s vote on comment `id`.
pub(crate) async fn vote_on(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    score: i16,
) -> Result<CommentScore, AppError> {
    current.require(Permission::Vote)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    if !(-1..=1).contains(&score) {
        return Err(AppError::BadRequest("A vote is 1, -1 or 0".into()));
    }
    let (comment, _) = visible_comment(state, current, id).await?;
    if comment.is_deleted {
        return Err(AppError::NotFound);
    }
    if comment.creator_id == Some(user.id) {
        return Err(AppError::Unprocessable(
            "You can't vote on your own comment.".into(),
        ));
    }
    let db = state.db.primary();
    comments::vote(db, user.id, id, score).await?;
    let comment = comments::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    Ok(CommentScore {
        score: comment.score,
        vote: score,
    })
}

/// Plain forms redirect back to the comment; the page script asks for
/// JSON to update the score in place.
async fn vote(
    page: Page,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<VoteForm>,
) -> Result<Response, AppError> {
    let result = vote_on(page.state(), &page.current, id, form.score).await?;
    let wants_json = headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"));
    if wants_json {
        return Ok(Json(result).into_response());
    }
    let comment = comments::by_id(page.state().db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Redirect::to(&url(&comment)).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct ReasonForm {
    #[serde(default)]
    reason: String,
}

/// Reports comment `id` to the moderators.
pub(crate) async fn report_comment(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    reason: &str,
) -> Result<(), AppError> {
    current.require(Permission::Flag)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(AppError::BadRequest(
            "Say what's wrong with the comment".into(),
        ));
    }
    if reason.chars().count() > REASON_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "The reason may be at most {REASON_MAX_LEN} characters"
        )));
    }
    let (comment, _) = visible_comment(state, current, id).await?;
    if comment.is_deleted {
        return Err(AppError::NotFound);
    }
    state.rate_limits.check_report(user.id).await?;
    match comments::report(state.db.primary(), id, user.id, reason).await {
        Ok(()) => {}
        Err(ReportError::Db(e)) => return Err(e.into()),
        Err(e) => return Err(AppError::BadRequest(e.to_string())),
    }
    tracing::info!(comment = id, user = user.name, "comment reported");
    Ok(())
}

async fn report(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<ReasonForm>,
) -> Result<Response, AppError> {
    report_comment(page.state(), &page.current, id, &form.reason).await?;
    let comment = comments::by_id(page.state().db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&url(&comment))).into_response())
}

/// Records a staff action on `comment`.
fn comment_action(
    actor: Option<i64>,
    kind: ActionKind,
    comment: &Comment,
    reports: u64,
) -> NewAction<'static> {
    let action = NewAction::new(actor, kind)
        .post(comment.post_id)
        .details(serde_json::json!({ "comment_id": comment.id, "reports": reports }));
    match comment.creator_id {
        Some(author) => action.user(author),
        None => action,
    }
}

/// Hides (`hidden`) or restores comment `id` as staff, logging it with
/// `reason`. Hiding upholds the comment's open reports.
pub(crate) async fn moderate(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    hidden: bool,
    reason: &str,
) -> Result<Comment, AppError> {
    current.require(Permission::ModerateComments)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let reason = reason.trim();
    if reason.chars().count() > REASON_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "The reason may be at most {REASON_MAX_LEN} characters"
        )));
    }
    let (comment, _) = visible_comment(state, current, id).await?;
    let mut tx = state.db.primary().begin().await?;
    if !comments::set_deleted(&mut *tx, id, hidden).await? {
        return Err(AppError::BadRequest(if hidden {
            "The comment is already hidden".into()
        } else {
            "The comment isn't hidden".into()
        }));
    }
    let reports = if hidden {
        comments::resolve_reports(&mut *tx, id, true, actor).await?
    } else {
        0
    };
    let kind = if hidden {
        ActionKind::CommentHide
    } else {
        ActionKind::CommentRestore
    };
    mod_actions::record(
        &mut *tx,
        comment_action(actor, kind, &comment, reports).reason(reason),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(comment = id, hidden, "comment moderated");
    Ok(comment)
}

async fn hide(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<ReasonForm>,
) -> Result<Response, AppError> {
    let comment = moderate(page.state(), &page.current, id, true, &form.reason).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&url(&comment))).into_response())
}

async fn restore(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    let comment = moderate(page.state(), &page.current, id, false, "").await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&url(&comment))).into_response())
}

#[derive(Debug, Deserialize)]
struct StickyForm {
    /// `1` pins the comment to the top of its post's comments, `0` unpins it.
    sticky: String,
}

/// Pins a comment to the top of its post's comments, or unpins it.
async fn sticky(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<StickyForm>,
) -> Result<Response, AppError> {
    let state = page.state();
    page.current.require(Permission::ModerateComments)?;
    let sticky = form.sticky == "1";
    let (comment, _) = visible_comment(state, &page.current, id).await?;
    if comment.is_deleted {
        return Err(AppError::BadRequest("The comment is hidden".into()));
    }
    let mut tx = state.db.primary().begin().await?;
    if comments::set_sticky(&mut *tx, id, sticky).await? {
        let kind = if sticky {
            ActionKind::CommentSticky
        } else {
            ActionKind::CommentUnsticky
        };
        let actor = page.current.user.as_ref().map(|u| u.id);
        mod_actions::record(&mut *tx, comment_action(actor, kind, &comment, 0)).await?;
    }
    tx.commit().await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&url(&comment))).into_response())
}

/// Dismisses comment `id`'s open reports, keeping the comment. Returns
/// how many there were.
pub(crate) async fn dismiss(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<u64, AppError> {
    current.require(Permission::ModerateComments)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let (comment, _) = visible_comment(state, current, id).await?;
    let mut tx = state.db.primary().begin().await?;
    let dismissed = comments::resolve_reports(&mut *tx, id, false, actor).await?;
    if dismissed == 0 {
        return Err(AppError::BadRequest(
            "The comment has no open reports".into(),
        ));
    }
    mod_actions::record(
        &mut *tx,
        comment_action(actor, ActionKind::CommentReportDismiss, &comment, dismissed),
    )
    .await?;
    tx.commit().await?;
    Ok(dismissed)
}

async fn dismiss_reports(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    dismiss(page.state(), &page.current, id).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/moderation/comments"),
    )
        .into_response())
}

/// Comments with open reports, oldest report first.
async fn report_queue(
    page: Page,
    Query(query): Query<crate::moderation::QueueQuery>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ModerateComments)?;
    let state = page.state();
    let db = state.db.primary();
    let reports = comments::open_reports(db, query.after.unwrap_or(0), REPORT_PAGE).await?;
    // Each comment's reports come together, oldest first.
    let mut ids: Vec<i64> = reports.iter().map(|r| r.comment_id).collect();
    ids.dedup();
    let more = (ids.len() == REPORT_PAGE as usize)
        .then(|| reports.iter().find(|r| Some(&r.comment_id) == ids.last()))
        .flatten()
        .map(|r| url_value(&format!("/moderation/comments?after={}", r.id)));
    let found = comments::by_ids(db, &ids).await?;
    let contexts = comment_contexts(state, &page.current, &found).await?;
    let rows: Vec<Value> = found
        .iter()
        .zip(contexts)
        .map(|(c, comment)| {
            let reasons: Vec<Value> = reports
                .iter()
                .filter(|r| r.comment_id == c.id)
                .map(|r| context! { by => r.creator_name, reason => r.reason })
                .collect();
            context! { comment => comment, reports => reasons }
        })
        .collect();
    Ok(page.render(
        "moderation_comments.html",
        context! { rows => rows, more_url => more },
    ))
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
        0,
        PAGE_SIZE + 1,
    )
    .await?;
    let has_next = found.len() > PAGE_SIZE as usize;
    found.truncate(PAGE_SIZE as usize);

    // Each comment beside its post's thumbnail, left out if blacklisted.
    let mut post_ids: Vec<i64> = found.iter().map(|c| c.post_id).collect();
    post_ids.dedup();
    let cards = crate::posts::grid(&page, db, &post_ids, None).await?;
    let contexts = comment_contexts(state, &page.current, &found).await?;
    let rows: Vec<Value> = found
        .iter()
        .zip(contexts)
        .map(|(c, comment)| {
            let card = cards
                .iter()
                .find(|(id, _)| *id == c.post_id)
                .map(|(_, card)| card);
            context! { comment => comment, card => card }
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

    async fn user_id(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn votes_reports_and_hiding(pool: PgPool) {
        let app = app(&pool).await;
        let post = post(&pool, "active").await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        let id = comments::create(&pool, post, user_id(&pool, "alice").await, "Hello", true)
            .await
            .unwrap();
        let vote = format!("/comments/{id}/vote");

        // Not on your own comment.
        assert_eq!(
            app.post_form(&vote, Some(&alice), &[], "score=1")
                .await
                .status,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let voted = app
            .post_form(
                &vote,
                Some(&bob),
                &[("accept", "application/json")],
                "score=-1",
            )
            .await;
        assert_eq!(voted.status, StatusCode::OK, "{}", voted.body);
        let json: serde_json::Value = serde_json::from_str(&voted.body).unwrap();
        assert_eq!(json, serde_json::json!({ "score": -1, "vote": -1 }));
        let page = app.get(&format!("/posts/{post}"), Some(&bob)).await;
        assert!(
            page.body
                .contains(&format!("action=\"/comments/{id}/vote\"")),
            "{}",
            page.body
        );
        assert!(
            !app.get(&format!("/posts/{post}"), Some(&alice))
                .await
                .body
                .contains(&vote)
        );

        // Low scores collapse.
        sqlx::query("UPDATE comments SET score = -5 WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            app.get(&format!("/posts/{post}"), None)
                .await
                .body
                .contains("Hidden for its low score")
        );

        // Reports go to the queue, for staff only.
        let report = format!("/comments/{id}/report");
        assert_eq!(
            app.post_form(&report, Some(&bob), &[], "reason=+")
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            app.post_form(&report, Some(&bob), &[], "reason=rude")
                .await
                .status,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            app.post_form(&report, Some(&bob), &[], "reason=rude")
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            app.get("/moderation/comments", Some(&bob)).await.status,
            StatusCode::FORBIDDEN
        );
        let queue = app.get("/moderation/comments", Some(&jan)).await;
        assert!(queue.body.contains("“rude”"), "{}", queue.body);
        assert_eq!(
            app.post(&format!("/comments/{id}/reports/dismiss"), Some(&jan), &[])
                .await
                .status,
            StatusCode::SEE_OTHER
        );
        assert!(
            app.get("/moderation/comments", Some(&jan))
                .await
                .body
                .contains("No reported comments.")
        );

        // Staff hide and restore anyone's comment, in the log.
        app.post_form(&report, Some(&bob), &[], "reason=still+rude")
            .await;
        assert_eq!(
            app.post_form(&format!("/comments/{id}/hide"), Some(&bob), &[], "reason=")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.post_form(
                &format!("/comments/{id}/hide"),
                Some(&jan),
                &[],
                "reason=spam"
            )
            .await
            .status,
            StatusCode::SEE_OTHER
        );
        assert!(
            !app.get(&format!("/posts/{post}"), None)
                .await
                .body
                .contains("Hello")
        );
        let upheld: String = sqlx::query_scalar(
            "SELECT status FROM comment_reports WHERE comment_id = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(upheld, "upheld");
        let staff_view = app.get(&format!("/posts/{post}"), Some(&jan)).await;
        assert!(staff_view.body.contains(&format!("/comments/{id}/restore")));
        app.post(&format!("/comments/{id}/restore"), Some(&jan), &[])
            .await;
        assert!(
            app.get(&format!("/posts/{post}"), None)
                .await
                .body
                .contains("Hello")
        );
        let logged: Vec<(String, String)> =
            sqlx::query_as("SELECT action, reason FROM mod_actions WHERE post_id = $1 ORDER BY id")
                .bind(post)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            logged,
            [
                ("comment_report.dismiss".to_owned(), String::new()),
                ("comment.hide".to_owned(), "spam".to_owned()),
                ("comment.restore".to_owned(), String::new()),
            ]
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn quiet_and_pinned_comments(pool: PgPool) {
        let app = app(&pool).await;
        let post = post(&pool, "active").await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        let posted = app
            .post_form(
                &format!("/posts/{post}/comments"),
                Some(&alice),
                &[],
                "body=Quietly&do_not_bump=1",
            )
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let quiet: (i64, bool) =
            sqlx::query_as("SELECT id, do_not_bump FROM comments WHERE body = 'Quietly'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(quiet.1);
        let bumped: Option<time::OffsetDateTime> =
            sqlx::query_scalar("SELECT last_comment_bumped_at FROM posts WHERE id = $1")
                .bind(post)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(bumped, None);
        app.post_form(
            &format!("/posts/{post}/comments"),
            Some(&alice),
            &[],
            "body=Loudly",
        )
        .await;

        let pin = format!("/comments/{}/sticky", quiet.0);
        assert_eq!(
            app.post_form(&pin, Some(&alice), &[], "sticky=1")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let page = app.get(&format!("/posts/{post}"), Some(&jan)).await.body;
        assert!(page.contains("Pin to top"), "{page}");
        let pinned = app.post_form(&pin, Some(&jan), &[], "sticky=1").await;
        assert_eq!(pinned.status, StatusCode::SEE_OTHER, "{}", pinned.body);
        let page = app.get(&format!("/posts/{post}"), Some(&alice)).await.body;
        assert!(page.contains(">pinned</span>"), "{page}");
        assert!(
            page.find("Quietly").unwrap() < page.find("Loudly").unwrap(),
            "{page}"
        );
        let logged: i64 =
            sqlx::query_scalar("SELECT count(*) FROM mod_actions WHERE action = 'comment.sticky'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(logged, 1);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn banned_users_cant_comment(pool: PgPool) {
        let app = app(&pool).await;
        let post = post(&pool, "active").await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        sqlx::query(
            "INSERT INTO bans (user_id, reason) SELECT id, 'spam' FROM users WHERE name = 'alice'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let refused = app
            .post_form(
                &format!("/posts/{post}/comments"),
                Some(&alice),
                &[],
                "body=hi",
            )
            .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert!(
            !app.get(&format!("/posts/{post}"), Some(&alice))
                .await
                .body
                .contains("id=\"new-comment\"")
        );
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
    async fn edits_go_through_the_spam_filter_and_rate_limit(pool: PgPool) {
        moekura_db::settings::set(
            &pool,
            "spam_filter",
            serde_json::json!({ "words": ["casino"] }),
        )
        .await
        .unwrap();
        let app = app(&pool).await;
        let post = post(&pool, "active").await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = comments::create(&pool, post, user_id(&pool, "alice").await, "Nice", true)
            .await
            .unwrap();
        let edit = format!("/comments/{id}/edit");

        let held = app
            .post_form(&edit, Some(&alice), &[], "body=Best+casino+deals")
            .await;
        assert_eq!(held.status, StatusCode::SEE_OTHER, "{}", held.body);
        assert_eq!(
            held.location.as_deref(),
            Some(&*format!("/posts/{post}#comments"))
        );
        let stored: (bool, Option<String>) =
            sqlx::query_as("SELECT is_deleted, held_reason FROM comments WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, (true, Some("contains “casino”".to_owned())));
        assert!(
            !app.get(&format!("/posts/{post}"), None)
                .await
                .body
                .contains("casino")
        );

        // Edits count against the comment rate limit like new comments.
        let other = comments::create(&pool, post, user_id(&pool, "alice").await, "Hi", true)
            .await
            .unwrap();
        let edit = format!("/comments/{other}/edit");
        for i in 0..4 {
            let body = format!("body=Hi+{i}");
            let saved = app.post_form(&edit, Some(&alice), &[], &body).await;
            assert_eq!(saved.status, StatusCode::SEE_OTHER, "{i}: {}", saved.body);
        }
        let refused = app
            .post_form(&edit, Some(&alice), &[], "body=Hi+again")
            .await;
        assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            refused.body.contains("You&#x27;re commenting too quickly"),
            "{}",
            refused.body
        );
        assert_eq!(
            comments::by_id(&pool, other).await.unwrap().unwrap().body,
            "Hi 3"
        );
        // Saving the same text again changes nothing, so counts for nothing.
        let unchanged = app.post_form(&edit, Some(&alice), &[], "body=Hi+3").await;
        assert_eq!(unchanged.status, StatusCode::SEE_OTHER);
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
            comments::create(&pool, visible, alice_id, &format!("number {i}."), true)
                .await
                .unwrap();
        }
        comments::create(&pool, pending, alice_id, "secret", true)
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
            comments::create(&pool, visible, alice_id, &format!("number {i}."), true)
                .await
                .unwrap();
        }
        let page = app.get(&format!("/posts/{visible}"), None).await;
        assert!(page.body.contains("number 54."));
        assert!(!page.body.contains("number 4."));
        assert!(page.body.contains("5 older comments"), "{}", page.body);
    }
}
