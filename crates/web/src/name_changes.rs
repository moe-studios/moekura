//! Changing names: users change their own at `/settings/name` every
//! [`NAME_CHANGE_DAYS`] days, and staff who can ban users rename those
//! below them from their profile. Old names stay on the profile, and old
//! links and `user:` searches still find the user.

use axum::extract::Path;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::context;
use moekura_core::accounts::{NAME_CHANGE_DAYS, UserName};
use moekura_core::moderation::{ActionKind, REASON_MAX_LEN};
use moekura_core::permissions::Permission;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::name_changes::{self, RenameError};
use moekura_db::users::{self, User};
use serde::Deserialize;
use time::{Duration, OffsetDateTime};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings/name", get(form).post(change_own))
        .route("/users/{name}/rename", post(rename_user))
}

/// Whether `current` may rename `user`: staff who can ban, someone
/// else ranked below them.
pub(crate) fn may_rename(state: &AppState, current: &CurrentUser, user: &User) -> bool {
    current.can(Permission::BanUsers)
        && current.user.as_ref().is_some_and(|me| me.id != user.id)
        && state
            .site
            .get()
            .role(user.role_id)
            .is_some_and(|role| current.role.outranks(role))
}

/// Renames `user` to `wanted`, by `changer`; the message says what's wrong.
async fn rename(
    state: &AppState,
    user: &User,
    wanted: &str,
    changer: Option<i64>,
) -> Result<String, AppError> {
    let name = UserName::parse(wanted.trim())
        .map_err(|e| AppError::Unprocessable(format!("The name {e}.")))?;
    if name.as_str() == user.name {
        return Err(AppError::Unprocessable("That's the name already.".into()));
    }
    // The tagger's, even before it has made its account.
    if state.config.tagger.reserves(name.as_str()) && !user.name.eq_ignore_ascii_case(name.as_str())
    {
        return Err(AppError::Unprocessable(format!("“{name}” is taken.")));
    }
    match name_changes::rename(state.db.primary(), user.id, name.as_str(), changer).await {
        Ok(()) => Ok(name.as_str().to_owned()),
        Err(RenameError::Taken) => Err(AppError::Unprocessable(format!("“{name}” is taken."))),
        Err(RenameError::NoUser) => Err(AppError::NotFound),
        Err(RenameError::Db(e)) => Err(e.into()),
    }
}

/// When `user` may next change their own name, if not yet.
async fn next_change(state: &AppState, user: &User) -> Result<Option<OffsetDateTime>, AppError> {
    let last = name_changes::last_own(state.db.primary(), user.id).await?;
    Ok(last
        .map(|at| at + Duration::days(NAME_CHANGE_DAYS))
        .filter(|next| *next > OffsetDateTime::now_utc()))
}

fn render_form(
    page: &Page,
    user: &User,
    next: Option<OffsetDateTime>,
    name: &str,
    error: Option<String>,
) -> Response {
    page.render_with_status(
        if error.is_some() {
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        } else {
            axum::http::StatusCode::OK
        },
        "name_change.html",
        context! {
            current => user.name,
            name => name,
            days => NAME_CHANGE_DAYS,
            next => next.map(crate::dates::day),
            error => error,
        },
    )
}

/// Users may change their own name unless banned: not to one that
/// mocks the ban, nor back from one staff gave them.
fn renamer(page: &Page) -> Result<User, AppError> {
    let user = page.current.user.clone().ok_or(AppError::Unauthorized)?;
    if page.current.ban.is_some() {
        return Err(AppError::Forbidden);
    }
    Ok(user)
}

async fn form(page: Page) -> Result<Response, AppError> {
    let user = renamer(&page)?;
    let next = next_change(page.state(), &user).await?;
    Ok(render_form(&page, &user, next, "", None))
}

#[derive(Debug, Default, Deserialize)]
struct NameForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    reason: String,
}

async fn change_own(
    page: Page,
    jar: CookieJar,
    Form(form): Form<NameForm>,
) -> Result<Response, AppError> {
    let user = renamer(&page)?;
    let state = page.state();
    if let Some(next) = next_change(state, &user).await? {
        let message = format!(
            "You can change your name once every {NAME_CHANGE_DAYS} days: next on {}.",
            crate::dates::day(next)
        );
        return Ok(render_form(
            &page,
            &user,
            Some(next),
            &form.name,
            Some(message),
        ));
    }
    match rename(state, &user, &form.name, Some(user.id)).await {
        Ok(name) => {
            tracing::info!(user = user.id, from = %user.name, to = %name, "name changed");
            Ok((
                flash::set(jar, Flash::Saved),
                Redirect::to(&format!("/users/{name}")),
            )
                .into_response())
        }
        Err(AppError::Unprocessable(message)) => {
            Ok(render_form(&page, &user, None, &form.name, Some(message)))
        }
        Err(error) => Err(error),
    }
}

async fn rename_user(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(form): Form<NameForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::BanUsers)?;
    let state = page.state();
    let user = users::by_name(state.db.primary(), &name)
        .await?
        .ok_or(AppError::NotFound)?;
    if !may_rename(state, &page.current, &user) {
        return Err(AppError::Forbidden);
    }
    let reason = form.reason.trim();
    if reason.chars().count() > REASON_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "The reason may be at most {REASON_MAX_LEN} characters"
        )));
    }
    let actor = page.current.user.as_ref().map(|u| u.id);
    let new = rename(state, &user, &form.name, actor)
        .await
        .map_err(|e| match e {
            AppError::Unprocessable(message) => AppError::BadRequest(message),
            other => other,
        })?;
    mod_actions::record(
        state.db.primary(),
        NewAction::new(actor, ActionKind::UserRename)
            .user(user.id)
            .reason(reason)
            .details(serde_json::json!({ "from": user.name, "to": new })),
    )
    .await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/users/{new}")),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn changing_names(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes()
                .merge(crate::users::routes())
                .merge(crate::danbooru::test_support::routes()),
        );
        let member = session_for(&pool, "member", SystemRole::Member).await;
        let other = session_for(&pool, "other", SystemRole::Member).await;
        let staff = session_for(&pool, "staff", SystemRole::Moderator).await;

        // Taken and invalid names are refused.
        let taken = app
            .post_form("/settings/name", Some(&member), &[], "name=OTHER")
            .await;
        assert_eq!(taken.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(taken.body.contains("is taken"), "{}", taken.body);
        // So is the tagger's, before it has an account.
        let tagger = app
            .post_form("/settings/name", Some(&member), &[], "name=TAGGER")
            .await;
        assert_eq!(tagger.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(tagger.body.contains("is taken"), "{}", tagger.body);
        let bad = app
            .post_form("/settings/name", Some(&member), &[], "name=a+b")
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

        let changed = app
            .post_form("/settings/name", Some(&member), &[], "name=renamed")
            .await;
        assert_eq!(changed.location.as_deref(), Some("/users/renamed"));

        // Not again for a while.
        let again = app
            .post_form("/settings/name", Some(&member), &[], "name=again")
            .await;
        assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(again.body.contains("once every 7 days"), "{}", again.body);
        assert!(
            app.get("/settings/name", Some(&member))
                .await
                .body
                .contains("you can next change it on")
        );

        // The old name leads to the new one, which remembers it.
        let old = app.get("/users/member", None).await;
        assert_eq!(old.status, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(old.location.as_deref(), Some("/users/renamed"));
        let profile = app.get("/users/renamed", None).await.body;
        assert!(
            profile.contains("<span>Formerly member</span>"),
            "{profile}"
        );
        assert!(!profile.contains("Rename renamed"));

        // Staff rename those below them, any time, in the log.
        assert!(
            app.get("/users/renamed", Some(&staff))
                .await
                .body
                .contains("Rename renamed")
        );
        assert_eq!(
            app.post_form("/users/renamed/rename", Some(&other), &[], "name=x1")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let renamed = app
            .post_form(
                "/users/renamed/rename",
                Some(&staff),
                &[],
                "name=fixed&reason=Impersonation",
            )
            .await;
        assert_eq!(renamed.location.as_deref(), Some("/users/fixed"));
        let (action, reason): (String, String) =
            sqlx::query_as("SELECT action, reason FROM mod_actions")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (action.as_str(), reason.as_str()),
            ("user.rename", "Impersonation")
        );
        assert_eq!(
            app.get("/users/member", None).await.location.as_deref(),
            Some("/users/fixed")
        );

        // Danbooru clients read the changes.
        let found: Value = serde_json::from_str(
            &app.get(
                "/user_name_change_requests.json?search[original_name]=member",
                None,
            )
            .await
            .body,
        )
        .unwrap();
        assert_eq!(found[0]["desired_name"], "renamed");
        assert_eq!(found.as_array().unwrap().len(), 1);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn banned_users_keep_their_names(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let member = session_for(&pool, "member", SystemRole::Member).await;
        let staff = session_for(&pool, "staff", SystemRole::Moderator).await;
        let renamed = app
            .post_form(
                "/users/member/rename",
                Some(&staff),
                &[],
                "name=fixed&reason=Impersonation",
            )
            .await;
        assert_eq!(renamed.location.as_deref(), Some("/users/fixed"));
        sqlx::query(
            "INSERT INTO bans (user_id, reason) SELECT id, 'spam' FROM users WHERE name = 'fixed'",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Not back to the old name, nor any other.
        assert_eq!(
            app.get("/settings/name", Some(&member)).await.status,
            StatusCode::FORBIDDEN
        );
        for name in ["member", "other"] {
            let refused = app
                .post_form(
                    "/settings/name",
                    Some(&member),
                    &[],
                    &format!("name={name}"),
                )
                .await;
            assert_eq!(refused.status, StatusCode::FORBIDDEN);
        }
        let name: String = sqlx::query_scalar("SELECT name::text FROM users WHERE name = 'fixed'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "fixed");
    }
}
