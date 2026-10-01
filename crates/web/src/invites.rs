//! Invites from the web, at `/invites`: those with *Invite people* make
//! single-use codes up to the site's quota, and those who manage users
//! make any kind and see and revoke everyone's. The list shows each
//! invite's uses, expiry and who signed up with it.

use std::time::Duration;

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::settings::{INVITE_QUOTA_DAYS, RegistrationMode};
use moekura_db::invites::{self, Invite, NewInvite};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

/// Invites per page.
const PAGE_SIZE: i64 = 50;
/// The longest note, in characters.
const NOTE_MAX_LEN: usize = 200;
/// Days a quota-limited invite may last, and lasts unless chosen.
const MAX_DAYS: u32 = 30;
const DEFAULT_DAYS: u32 = 7;
/// Limits for those who manage users.
const MAX_USES: i32 = 1000;
const MAX_MANAGER_DAYS: u32 = 3650;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/invites", get(index).post(create))
        .route("/invites/{id}/revoke", post(revoke))
}

/// Whether `current` may make invites at all.
pub(crate) fn may_invite(current: &CurrentUser) -> bool {
    current.is_logged_in()
        && (current.can(Permission::InviteUsers) || current.can(Permission::ManageUsers))
}

fn user_id(current: &CurrentUser) -> Result<i64, AppError> {
    current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)
}

/// What the person can make: `None` without limit, else how many more
/// invites the quota allows now.
async fn remaining(state: &AppState, current: &CurrentUser) -> Result<Option<i64>, AppError> {
    if current.can(Permission::ManageUsers) {
        return Ok(None);
    }
    let made =
        invites::made_since(state.db.primary(), user_id(current)?, INVITE_QUOTA_DAYS).await?;
    let quota = i64::from(state.site.get().settings.invite_quota);
    Ok(Some((quota - made).max(0)))
}

fn invite_context(invite: &Invite, current: &CurrentUser) -> Value {
    let mine =
        invite.created_by.is_some() && invite.created_by == current.user.as_ref().map(|u| u.id);
    let now = time::OffsetDateTime::now_utc();
    let state = if invite.revoked_at.is_some() {
        "revoked"
    } else if invite.uses >= invite.max_uses {
        "used up"
    } else if invite.expires_at.is_some_and(|at| at <= now) {
        "expired"
    } else {
        "open"
    };
    context! {
        id => invite.id,
        creator => invite.creator_name,
        note => invite.note,
        uses => invite.uses,
        max_uses => invite.max_uses,
        expires => invite.expires_at.map(crate::dates::day),
        created => crate::dates::day(invite.created_at),
        used_by => invite.used_by,
        state => state,
        can_revoke => invite.is_usable() && (mine || current.can(Permission::ManageUsers)),
    }
}

#[derive(Debug, Deserialize)]
struct InviteForm {
    #[serde(default = "one")]
    uses: String,
    #[serde(default = "default_days")]
    expires_days: String,
    #[serde(default)]
    note: String,
}

fn one() -> String {
    "1".into()
}

fn default_days() -> String {
    DEFAULT_DAYS.to_string()
}

impl Default for InviteForm {
    fn default() -> Self {
        Self {
            uses: one(),
            expires_days: default_days(),
            note: String::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    page: Option<i64>,
}

/// A link that signs up with `code`.
fn signup_link(state: &AppState, code: &str) -> String {
    let path = format!(
        "/register?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("invite", code)
            .finish()
    );
    state
        .config
        .server
        .public_url
        .join(&path)
        .map_or(path, String::from)
}

async fn render(
    page: &Page,
    number: i64,
    form: &InviteForm,
    created: Option<&str>,
    error: Option<String>,
) -> Result<Response, AppError> {
    let state = page.state();
    let current = &page.current;
    let manager = current.can(Permission::ManageUsers);
    let mine = (!manager).then(|| user_id(current)).transpose()?;
    let mut found = invites::list(
        state.db.primary(),
        mine,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let more = found.len() > PAGE_SIZE as usize;
    found.truncate(PAGE_SIZE as usize);
    let remaining = remaining(state, current).await?;
    let link = |n: i64| {
        if n == 1 {
            "/invites".to_owned()
        } else {
            format!("/invites?page={n}")
        }
    };
    Ok(page.render_with_status(
        if error.is_some() {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::OK
        },
        "invites.html",
        context! {
            invites => found.iter().map(|i| invite_context(i, current)).collect::<Vec<_>>(),
            manager => manager,
            remaining => remaining,
            quota => state.site.get().settings.invite_quota,
            quota_days => INVITE_QUOTA_DAYS,
            invite_mode => state.site.get().settings.registration_mode == RegistrationMode::Invite,
            created => created.map(|code| context! { code => code, link => signup_link(state, code) }),
            error => error,
            form => context! { uses => form.uses, expires_days => form.expires_days, note => form.note },
            max_days => if manager { MAX_MANAGER_DAYS } else { MAX_DAYS },
            max_uses => MAX_USES,
            note_max => NOTE_MAX_LEN,
            previous_url => (number > 1).then(|| link(number - 1)),
            next_url => more.then(|| link(number + 1)),
        },
    ))
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    if !page.current.is_logged_in() {
        return Err(AppError::Unauthorized);
    }
    if !may_invite(&page.current) {
        return Err(AppError::Forbidden);
    }
    let number = query.page.unwrap_or(1).clamp(1, 1000);
    render(&page, number, &InviteForm::default(), None, None).await
}

/// The invite `form` asks for, or why it can't be made.
fn checked(form: &InviteForm, manager: bool, creator: i64) -> Result<NewInvite, String> {
    let note = form.note.trim();
    if note.chars().count() > NOTE_MAX_LEN {
        return Err(format!(
            "The note may be at most {NOTE_MAX_LEN} characters."
        ));
    }
    let days: u32 = form
        .expires_days
        .trim()
        .parse()
        .map_err(|_| "Say how many days the invite lasts.".to_owned())?;
    let max_uses = if manager {
        let uses: i32 = form
            .uses
            .trim()
            .parse()
            .map_err(|_| "Say how many times the invite can be used.".to_owned())?;
        if !(1..=MAX_USES).contains(&uses) {
            return Err(format!("An invite can be used 1 to {MAX_USES} times."));
        }
        if days > MAX_MANAGER_DAYS {
            return Err(format!(
                "An invite can last at most {MAX_MANAGER_DAYS} days."
            ));
        }
        uses
    } else {
        if !(1..=MAX_DAYS).contains(&days) {
            return Err(format!("An invite can last 1 to {MAX_DAYS} days."));
        }
        1
    };
    Ok(NewInvite {
        created_by: Some(creator),
        max_uses,
        // Only those who manage users make invites that never expire.
        expires_in: (days > 0).then(|| Duration::from_secs(u64::from(days) * 86_400)),
        note: note.to_owned(),
    })
}

async fn create(page: Page, Form(form): Form<InviteForm>) -> Result<Response, AppError> {
    if !may_invite(&page.current) {
        return Err(AppError::Forbidden);
    }
    let state = page.state();
    let creator = user_id(&page.current)?;
    let manager = page.current.can(Permission::ManageUsers);
    if remaining(state, &page.current).await? == Some(0) {
        let message = format!(
            "You've made as many invites as you can for now ({} every {INVITE_QUOTA_DAYS} days).",
            state.site.get().settings.invite_quota
        );
        return render(&page, 1, &form, None, Some(message)).await;
    }
    let invite = match checked(&form, manager, creator) {
        Ok(invite) => invite,
        Err(message) => return render(&page, 1, &form, None, Some(message)).await,
    };
    let code = invites::create(state.db.primary(), invite).await?;
    tracing::info!(user = creator, "invite created");
    render(&page, 1, &InviteForm::default(), Some(&code), None).await
}

async fn revoke(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    if !may_invite(&page.current) {
        return Err(AppError::Forbidden);
    }
    let state = page.state();
    let invite = invites::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    let mine = invite.created_by.is_some()
        && invite.created_by == page.current.user.as_ref().map(|u| u.id);
    if !mine && !page.current.can(Permission::ManageUsers) {
        return Err(AppError::Forbidden);
    }
    invites::revoke(state.db.primary(), id).await?;
    tracing::info!(invite = id, "invite revoked");
    Ok((flash::set(jar, Flash::Saved), Redirect::to("/invites")).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::json;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    /// The code in a page that just made an invite.
    fn code_in(body: &str) -> String {
        let tag = "<code class=\"secret\">";
        let start = body.find(tag).expect(body) + tag.len();
        body[start..][..body[start..].find('<').unwrap()].to_owned()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn inviting_from_the_web(pool: PgPool) {
        moekura_db::settings::set(&pool, "registration_mode", json!("invite"))
            .await
            .unwrap();
        moekura_db::settings::set(&pool, "invite_quota", json!(2))
            .await
            .unwrap();
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes()
                .merge(crate::account::routes())
                .merge(crate::users::routes()),
        );
        let member = session_for(&pool, "member", SystemRole::Member).await;
        let moderator = session_for(&pool, "moderator", SystemRole::Moderator).await;
        let admin = session_for(&pool, "admin", SystemRole::Admin).await;

        assert_eq!(
            app.get("/invites", Some(&member)).await.status,
            StatusCode::FORBIDDEN
        );
        assert!(
            !app.get("/settings", Some(&member))
                .await
                .body
                .contains("href=\"/invites\"")
        );

        // Moderators make single-use invites, up to the quota.
        let page = app.get("/invites", Some(&moderator)).await;
        assert!(page.body.contains("2 left now"), "{}", page.body);
        assert!(!page.body.contains("name=\"uses\""));
        let too_long = app
            .post_form("/invites", Some(&moderator), &[], "expires_days=31")
            .await;
        assert_eq!(too_long.status, StatusCode::UNPROCESSABLE_ENTITY);
        let made = app
            .post_form(
                "/invites",
                Some(&moderator),
                &[],
                "uses=50&expires_days=7&note=For+Alice",
            )
            .await;
        assert_eq!(made.status, StatusCode::OK);
        assert!(made.body.contains("register?invite="), "{}", made.body);
        let code = code_in(&made.body);
        app.post_form("/invites", Some(&moderator), &[], "expires_days=1")
            .await;
        let over = app
            .post_form("/invites", Some(&moderator), &[], "expires_days=1")
            .await;
        assert_eq!(over.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(over.body.contains("as many invites as you can"));

        // Someone signs up with it; it's used up and remembered.
        let signup = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("name", "alice"),
                ("password", "correct horse"),
                ("password_confirm", "correct horse"),
                ("invite", code.as_str()),
            ])
            .finish();
        let registered = app.post_form("/register", None, &[], &signup).await;
        assert_eq!(
            registered.status,
            StatusCode::SEE_OTHER,
            "{}",
            registered.body
        );
        let list = app.get("/invites", Some(&moderator)).await.body;
        assert!(list.contains("For Alice"));
        assert!(list.contains("1 of 1"), "{list}");
        assert!(list.contains("href=\"/users/alice\""));
        assert!(list.contains("used up"));
        assert!(
            app.get("/users/alice", None)
                .await
                .body
                .contains("<dt>Invited by</dt><dd><a href=\"/users/moderator\">moderator</a>")
        );

        // Admins make any kind, see everyone's and revoke them.
        let many = app
            .post_form("/invites", Some(&admin), &[], "uses=3&expires_days=0")
            .await;
        assert_eq!(many.status, StatusCode::OK);
        let all = app.get("/invites", Some(&admin)).await.body;
        assert!(all.contains("href=\"/users/moderator\"") && all.contains("0 of 3"));
        let id: i64 = sqlx::query_scalar(
            "SELECT id FROM invites WHERE uses = 0 AND created_by =
               (SELECT id FROM users WHERE name = 'moderator')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            app.post_form(&format!("/invites/{id}/revoke"), Some(&member), &[], "")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let revoked = app
            .post_form(&format!("/invites/{id}/revoke"), Some(&admin), &[], "")
            .await;
        assert_eq!(revoked.location.as_deref(), Some("/invites"));
        assert!(
            app.get("/invites", Some(&moderator))
                .await
                .body
                .contains("revoked")
        );
    }
}
