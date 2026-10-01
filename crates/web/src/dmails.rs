//! Private messages between users ("dmails"): inbox and sent folders,
//! reading, deleting, reporting to staff, and blocking senders. Staff see
//! reported messages only, under Moderation.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_db::dmails::{self, Dmail, Folder, SendError};
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::users::{self, UserStatus};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::url_value;

/// Messages per page.
const PAGE_SIZE: i64 = 50;
/// The longest title, in characters.
pub(crate) const TITLE_MAX_LEN: usize = 250;
/// The longest message, in characters.
pub(crate) const BODY_MAX_LEN: usize = 50_000;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/dmails", get(index).post(create))
        .route("/dmails/new", get(new_form))
        .route("/dmails/mark_all_read", post(mark_all_read))
        .route("/dmails/{id}", get(show))
        .route("/dmails/{id}/{action}", post(act))
        .route("/users/{name}/block", post(block))
        .route("/moderation/dmails", get(reported))
        .route("/moderation/dmails/{id}/settle", post(settle))
}

/// The logged-in user, who may read their messages.
fn owner(current: &CurrentUser) -> Result<i64, AppError> {
    current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)
}

fn dmail_context(d: &Dmail, html: bool) -> Value {
    context! {
        id => d.id,
        url => url_value(&format!("/dmails/{}", d.id)),
        from => d.from_name,
        to => d.to_name,
        sent => d.from_id == Some(d.owner_id),
        title => d.title,
        html => html.then(|| Value::from_safe_string(markup::render(&d.body))),
        read => d.is_read,
        deleted => d.is_deleted,
        reported => d.reported_at.is_some(),
        report_reason => d.report_reason,
        date => crate::dates::day(d.created_at),
        time => crate::dates::clock(d.created_at),
    }
}

/// Checks a message's title and text.
pub(crate) fn clean(title: &str, body: &str) -> Result<(String, String), AppError> {
    let title = title.trim().to_owned();
    let body = body.replace("\r\n", "\n").trim_end().to_owned();
    if title.is_empty() || body.trim().is_empty() {
        return Err(AppError::Unprocessable(
            "A message needs a title and some text.".into(),
        ));
    }
    if title.chars().count() > TITLE_MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "The title can be at most {TITLE_MAX_LEN} characters long."
        )));
    }
    if body.chars().count() > BODY_MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "The message can be at most {BODY_MAX_LEN} characters long."
        )));
    }
    Ok((title, body))
}

/// Sends a message from `current` to the user called `to`.
pub(crate) async fn send(
    state: &AppState,
    current: &CurrentUser,
    to: &str,
    title: &str,
    body: &str,
) -> Result<dmails::Sent, AppError> {
    current.require(Permission::SendMessages)?;
    let from = owner(current)?;
    let (title, body) = clean(title, body)?;
    let db = state.db.primary();
    let recipient = users::by_name(db, to.trim())
        .await?
        .filter(|u| u.status == UserStatus::Active)
        .ok_or_else(|| {
            AppError::Unprocessable(format!("There's no user called “{}”.", to.trim()))
        })?;
    state.rate_limits.check_dmail(from).await?;
    let sent = dmails::send(db, from, recipient.id, &title, &body)
        .await
        .map_err(|e| match e {
            SendError::Blocked => AppError::Unprocessable(format!(
                "{} doesn't take messages from you.",
                recipient.name
            )),
            SendError::Db(e) => e.into(),
        })?;
    crate::notifications::notify(
        state,
        &[recipient.id],
        moekura_db::notifications::Kind::Message,
        Some(from),
        &format!("“{title}”"),
        &format!("/dmails/{}", sent.recipient_copy),
    )
    .await;
    Ok(sent)
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    #[serde(default)]
    folder: String,
    #[serde(default)]
    unread: String,
    page: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    let me = owner(&page.current)?;
    let db = page.state().db.primary();
    let folder = match query.folder.as_str() {
        "sent" => Folder::Sent,
        "all" => Folder::All,
        _ => Folder::Inbox,
    };
    let unread = query.unread == "1";
    let number = query.page.unwrap_or(1).max(1);
    let mut found = dmails::list(
        db,
        me,
        folder,
        unread,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let more = found.len() > PAGE_SIZE as usize;
    found.truncate(PAGE_SIZE as usize);
    let folder_name = match folder {
        Folder::Inbox => "inbox",
        Folder::Sent => "sent",
        Folder::All => "all",
    };
    let list_url = |n: i64| {
        url_value(&format!(
            "/dmails?folder={folder_name}{}&page={n}",
            if unread { "&unread=1" } else { "" }
        ))
    };
    Ok(page.render(
        "dmails.html",
        context! {
            folder => folder_name,
            unread => unread,
            dmails => found.iter().map(|d| dmail_context(d, false)).collect::<Vec<_>>(),
            blocked => dmails::blocked(db, me).await?,
            can_send => page.current.can(Permission::SendMessages),
            previous_url => (number > 1).then(|| list_url(number - 1)),
            next_url => more.then(|| list_url(number + 1)),
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct NewQuery {
    #[serde(default)]
    to: String,
    /// A message to answer.
    reply: Option<i64>,
}

fn form_context(to: &str, title: &str, body: &str, error: Option<String>) -> Value {
    context! { to => to, title => title, body => body, error => error, max_len => BODY_MAX_LEN }
}

async fn new_form(page: Page, Query(query): Query<NewQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::SendMessages)?;
    let me = owner(&page.current)?;
    let (to, title, body) = match query.reply {
        Some(id) => {
            let d = dmails::by_id(page.state().db.primary(), me, id)
                .await?
                .ok_or(AppError::NotFound)?;
            let to = if d.from_id == Some(me) {
                d.to_name.clone()
            } else {
                d.from_name.clone()
            };
            let title = if d.title.starts_with("Re: ") {
                d.title.clone()
            } else {
                format!("Re: {}", d.title)
            };
            let author = d.from_name.as_deref().unwrap_or("someone");
            (
                to.unwrap_or_default(),
                title,
                markup::quote(author, &d.body),
            )
        }
        None => (query.to, String::new(), String::new()),
    };
    Ok(page.render("dmail_new.html", form_context(&to, &title, &body, None)))
}

#[derive(Debug, Deserialize)]
struct NewForm {
    #[serde(default)]
    to: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<NewForm>,
) -> Result<Response, AppError> {
    match send(
        page.state(),
        &page.current,
        &form.to,
        &form.title,
        &form.body,
    )
    .await
    {
        Ok(_) => Ok((
            flash::set(jar, Flash::Saved),
            Redirect::to("/dmails?folder=sent"),
        )
            .into_response()),
        Err(AppError::Unprocessable(message)) => Ok(page.render_with_status(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "dmail_new.html",
            form_context(&form.to, &form.title, &form.body, Some(message)),
        )),
        Err(error) => Err(error),
    }
}

async fn show(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let me = owner(&page.current)?;
    let db = page.state().db.primary();
    let d = dmails::by_id(db, me, id).await?.ok_or(AppError::NotFound)?;
    if !d.is_read {
        dmails::set_read(db, me, id, true).await?;
        moekura_db::notifications::read_url(db, me, &format!("/dmails/{id}")).await?;
    }
    let other = if d.from_id == Some(me) {
        d.to_name.clone()
    } else {
        d.from_name.clone()
    };
    let blocked = match d.from_id.filter(|&f| f != me) {
        Some(from) => dmails::is_blocked(db, me, from).await?,
        None => false,
    };
    Ok(page.render(
        "dmail.html",
        context! {
            dmail => dmail_context(&d, true),
            other => other,
            blocked => blocked,
            can_send => page.current.can(Permission::SendMessages),
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct ReportForm {
    #[serde(default)]
    reason: String,
}

async fn act(
    page: Page,
    jar: CookieJar,
    Path((id, action)): Path<(i64, String)>,
    Form(form): Form<ReportForm>,
) -> Result<Response, AppError> {
    let me = owner(&page.current)?;
    let db = page.state().db.primary();
    let done = match action.as_str() {
        "delete" => dmails::set_deleted(db, me, id, true).await?,
        "restore" => dmails::set_deleted(db, me, id, false).await?,
        "unread" => dmails::set_read(db, me, id, false).await?,
        "report" => {
            let reason = form.reason.trim();
            if reason.chars().count() > 1000 {
                return Err(AppError::Unprocessable(
                    "The reason can be at most 1000 characters long.".into(),
                ));
            }
            dmails::report(db, me, id, reason).await?
        }
        _ => return Err(AppError::NotFound),
    };
    if !done {
        return Err(AppError::NotFound);
    }
    let back = if action == "report" || action == "restore" {
        format!("/dmails/{id}")
    } else {
        "/dmails".to_owned()
    };
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&back)).into_response())
}

async fn mark_all_read(page: Page, jar: CookieJar) -> Result<Response, AppError> {
    let me = owner(&page.current)?;
    dmails::mark_all_read(page.state().db.primary(), me).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to("/dmails")).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct BlockForm {
    /// `1` to block, `0` to unblock.
    #[serde(default)]
    blocked: String,
    /// Where to go back to: a message's page.
    back: Option<i64>,
}

async fn block(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(form): Form<BlockForm>,
) -> Result<Response, AppError> {
    let me = owner(&page.current)?;
    let db = page.state().db.primary();
    let other = users::by_name(db, &name).await?.ok_or(AppError::NotFound)?;
    if other.id == me {
        return Err(AppError::Unprocessable("You can't block yourself.".into()));
    }
    dmails::set_blocked(db, me, other.id, form.blocked == "1").await?;
    let back = form.back.map_or_else(
        || format!("/users/{}", other.name),
        |id| format!("/dmails/{id}"),
    );
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&back)).into_response())
}

async fn reported(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ModerateComments)?;
    let found = dmails::reported(page.state().db.primary(), 0, 100).await?;
    Ok(page.render(
        "moderation_dmails.html",
        context! {
            dmails => found.iter().map(|d| dmail_context(d, true)).collect::<Vec<_>>(),
        },
    ))
}

async fn settle(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    page.current.require(Permission::ModerateComments)?;
    let db = page.state().db.primary();
    let d = dmails::by_id_any(db, id).await?.ok_or(AppError::NotFound)?;
    if !dmails::settle_report(db, id).await? {
        return Err(AppError::NotFound);
    }
    let mut action = NewAction::new(
        page.current.user.as_ref().map(|u| u.id),
        ActionKind::DmailReportSettle,
    )
    .details(serde_json::json!({ "dmail_id": id }));
    if let Some(from) = d.from_id {
        action = action.user(from);
    }
    mod_actions::record(db, action).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/moderation/dmails"),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn messages(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::posts::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let staff = session_for(&pool, "staff", SystemRole::Moderator).await;
        assert_eq!(app.get("/dmails", None).await.status, StatusCode::SEE_OTHER);

        let nobody = app
            .post_form("/dmails", Some(&alice), &[], "to=carol&title=Hi&body=Hello")
            .await;
        assert_eq!(nobody.status, StatusCode::UNPROCESSABLE_ENTITY);
        let sent = app
            .post_form(
                "/dmails",
                Some(&alice),
                &[],
                "to=Bob&title=Hi&body=Hello+%5B%5Bcat%5D%5D",
            )
            .await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);

        let home = app.get("/", Some(&bob)).await.body;
        assert!(home.contains("count-badge\">1"), "{home}");
        let inbox = app.get("/dmails", Some(&bob)).await.body;
        let id: i64 = inbox
            .split("href=\"/dmails/")
            .skip(1)
            .find_map(|rest| rest.split('"').next()?.parse().ok())
            .expect(&inbox);
        // Only the owner reads their copy.
        assert_eq!(
            app.get(&format!("/dmails/{id}"), Some(&alice)).await.status,
            StatusCode::NOT_FOUND
        );
        let read = app.get(&format!("/dmails/{id}"), Some(&bob)).await.body;
        assert!(read.contains("href=\"/wiki/cat\""), "{read}");
        assert!(!app.get("/", Some(&bob)).await.body.contains("count-badge"));

        // Reported messages reach staff, and only those.
        assert_eq!(
            app.get("/moderation/dmails", Some(&staff))
                .await
                .body
                .matches("Hello")
                .count(),
            0
        );
        app.post_form(
            &format!("/dmails/{id}/report"),
            Some(&bob),
            &[],
            "reason=spam",
        )
        .await;
        let queue = app.get("/moderation/dmails", Some(&staff)).await.body;
        assert!(queue.contains("Hello") && queue.contains("spam"), "{queue}");
        assert_eq!(
            app.get("/moderation/dmails", Some(&bob)).await.status,
            StatusCode::FORBIDDEN
        );
        let settled = app
            .post(
                &format!("/moderation/dmails/{id}/settle"),
                Some(&staff),
                &[],
            )
            .await;
        assert_eq!(settled.status, StatusCode::SEE_OTHER);

        // Blocked senders are refused.
        app.post_form("/users/alice/block", Some(&bob), &[], "blocked=1")
            .await;
        let refused = app
            .post_form("/dmails", Some(&alice), &[], "to=bob&title=Hi&body=Again")
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            refused.body.contains("doesn&#x27;t take messages"),
            "{}",
            refused.body
        );
    }
}
