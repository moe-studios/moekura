//! Everything that emails a link: confirming an address (after
//! registering, or changing it), resetting a forgotten password. Also the
//! account settings page, where the address and password are changed.
//!
//! Without `[mail]`, the pages that need it answer 404 and aren't linked.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::context;
use moekura_core::accounts;
use moekura_core::jobs::SendMail;
use moekura_core::settings::RegistrationMode;
use moekura_db::account_tokens::{self, Purpose};
use moekura_db::accounts::PasswordChangeError;
use moekura_db::users::{self, InsertError, User, UserStatus};
use moekura_db::{jobs, sessions};
use serde::Deserialize;
use sqlx::PgConnection;

use crate::AppState;
use crate::auth::{RequestInfo, SESSION_COOKIE};
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

/// How long a link to confirm an address works.
const VERIFY_TTL: Duration = Duration::from_secs(48 * 3600);
/// How long a password reset link works.
const RESET_TTL: Duration = Duration::from_secs(3600);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/verify-email", get(verify))
        .route("/verify-email/resend", get(resend_form).post(resend))
        .route("/forgot-password", get(forgot_form).post(forgot))
        .route("/reset-password", get(reset_form).post(reset))
        .route("/settings/account", get(account))
        .route("/settings/account/email", post(change_email))
        .route("/settings/account/password", post(change_password))
}

pub(crate) fn mail_enabled(state: &AppState) -> bool {
    state.config.mail.is_enabled()
}

/// Whether new accounts must confirm their address before logging in.
pub(crate) fn verification_required(state: &AppState) -> bool {
    mail_enabled(state) && state.site.get().settings.email_verification
}

fn require_mail(state: &AppState) -> Result<(), AppError> {
    if mail_enabled(state) {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

/// `path` (with its query) on the public site, for links in messages.
fn link(state: &AppState, path: &str) -> String {
    state
        .config
        .server
        .public_url
        .join(path)
        .map_or_else(|_| path.to_owned(), String::from)
}

async fn queue(
    conn: &mut PgConnection,
    to: &str,
    subject: String,
    body: String,
) -> sqlx::Result<()> {
    let mail = SendMail {
        to: to.to_owned(),
        subject,
        body,
    };
    jobs::enqueue(conn, &mail).await?;
    Ok(())
}

/// Emails `email` a link that confirms it as `user`'s address. Call it
/// in the transaction that needs it, so the message only goes out if
/// that commits.
pub(crate) async fn send_verification(
    conn: &mut PgConnection,
    state: &AppState,
    user: &User,
    email: &str,
) -> sqlx::Result<()> {
    let token =
        account_tokens::issue(conn, user.id, Purpose::VerifyEmail, email, VERIFY_TTL).await?;
    let site = state.site.get().settings.site_name.clone();
    let url = link(state, &format!("/verify-email?token={token}"));
    let body = format!(
        "Hi {name},\n\n\
         Follow this link to confirm {email} as the address of your account on {site}:\n\n\
         {url}\n\n\
         The link works for 48 hours. If you didn't ask for this, you can ignore this message.\n",
        name = user.name,
    );
    queue(
        conn,
        email,
        format!("Confirm your email address for {site}"),
        body,
    )
    .await
}

async fn send_reset(
    conn: &mut PgConnection,
    state: &AppState,
    user: &User,
    email: &str,
) -> sqlx::Result<()> {
    let token =
        account_tokens::issue(conn, user.id, Purpose::ResetPassword, email, RESET_TTL).await?;
    let site = state.site.get().settings.site_name.clone();
    let url = link(state, &format!("/reset-password?token={token}"));
    let body = format!(
        "Hi {name},\n\n\
         Someone asked to reset the password of your account on {site}. \
         Follow this link to choose a new one:\n\n\
         {url}\n\n\
         The link works for an hour. If you didn't ask for this, you can ignore this message: \
         your password stays as it is.\n",
        name = user.name,
    );
    queue(conn, email, format!("Reset your password on {site}"), body).await
}

/// After an account's address is confirmed: an account waiting for that
/// moves on, to approval if the site wants it.
async fn finish_signup(
    conn: &mut PgConnection,
    state: &AppState,
    user: &User,
) -> sqlx::Result<UserStatus> {
    if user.status != UserStatus::Unverified {
        return Ok(user.status);
    }
    let status = match state.site.get().settings.registration_mode {
        RegistrationMode::Approval => UserStatus::Pending,
        _ => UserStatus::Active,
    };
    users::set_status(&mut *conn, user.id, status).await?;
    tracing::info!(
        user_id = user.id,
        ?status,
        "email address confirmed at signup"
    );
    Ok(status)
}

fn expired_link() -> AppError {
    AppError::BadRequest(
        "This link has expired or was already used. Ask for a new one and try again.".into(),
    )
}

// ---- confirming an address -------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: String,
}

async fn verify(
    State(state): State<AppState>,
    page: Page,
    jar: CookieJar,
    Query(query): Query<TokenQuery>,
) -> Result<Response, AppError> {
    let mut tx = state.db.primary().begin().await?;
    let redeemed = account_tokens::redeem(&mut *tx, Purpose::VerifyEmail, &query.token)
        .await?
        .ok_or_else(expired_link)?;
    let user = users::by_id(&mut *tx, redeemed.user_id)
        .await?
        .ok_or_else(expired_link)?;
    users::set_email(&mut *tx, user.id, Some(&redeemed.email), true)
        .await
        .map_err(|e| match e {
            InsertError::EmailTaken => {
                AppError::BadRequest("That address is now used by another account.".into())
            }
            InsertError::NameTaken | InsertError::Db(_) => AppError::Internal(e.to_string()),
        })?;
    let status = finish_signup(&mut tx, &state, &user).await?;
    tx.commit().await?;
    tracing::info!(user_id = user.id, "email address confirmed");

    let (message, to) = match (status, page.current.is_logged_in()) {
        (UserStatus::Pending, _) => (Flash::AwaitingApproval, "/"),
        (_, true) => (Flash::EmailConfirmed, "/settings/account"),
        (_, false) => (Flash::EmailConfirmed, "/login"),
    };
    Ok((flash::set(jar, message), Redirect::to(to)).into_response())
}

async fn resend_form(page: Page) -> Result<Response, AppError> {
    require_mail(page.state())?;
    Ok(page.render("resend_verification.html", context! {}))
}

#[derive(Debug, Deserialize)]
struct NameForm {
    #[serde(default)]
    name: String,
}

/// Sends the confirmation link again to an account still waiting for
/// one. Says the same whatever the name, so it reveals nothing.
async fn resend(
    State(state): State<AppState>,
    jar: CookieJar,
    info: RequestInfo,
    Form(form): Form<NameForm>,
) -> Result<Response, AppError> {
    require_mail(&state)?;
    let name = form.name.trim();
    state.rate_limits.check_mail(info.ip, name).await?;
    let db = state.db.primary();
    if let Some(user) = users::by_name(db, name).await?
        && user.status == UserStatus::Unverified
        && let Some(email) = user.email.clone()
    {
        let mut tx = db.begin().await?;
        send_verification(&mut tx, &state, &user, &email).await?;
        tx.commit().await?;
    }
    Ok((flash::set(jar, Flash::CheckEmail), Redirect::to("/login")).into_response())
}

// ---- forgotten passwords --------------------------------------------------

async fn forgot_form(page: Page) -> Result<Response, AppError> {
    require_mail(page.state())?;
    Ok(page.render("forgot_password.html", context! { sent => false }))
}

#[derive(Debug, Deserialize)]
struct EmailForm {
    #[serde(default)]
    email: String,
}

/// Emails a reset link if an account uses the address. The answer is the
/// same either way, so the form can't be used to find out who has an
/// account.
async fn forgot(
    State(state): State<AppState>,
    page: Page,
    info: RequestInfo,
    Form(form): Form<EmailForm>,
) -> Result<Response, AppError> {
    require_mail(&state)?;
    let email = form.email.trim();
    state.rate_limits.check_mail(info.ip, email).await?;
    let db = state.db.primary();
    if !email.is_empty()
        && let Some(user) = users::by_email(db, email).await?
        && user.status != UserStatus::Deactivated
        && let Some(address) = user.email.clone()
    {
        let mut tx = db.begin().await?;
        send_reset(&mut tx, &state, &user, &address).await?;
        tx.commit().await?;
        tracing::info!(user_id = user.id, "password reset requested");
    }
    Ok(page.render(
        "forgot_password.html",
        context! { sent => true, email => email },
    ))
}

async fn reset_form(page: Page, Query(query): Query<TokenQuery>) -> Result<Response, AppError> {
    let db = page.state().db.primary();
    account_tokens::peek(db, Purpose::ResetPassword, &query.token)
        .await?
        .ok_or_else(expired_link)?;
    Ok(page.render(
        "reset_password.html",
        context! { token => query.token, error => None::<String> },
    ))
}

#[derive(Debug, Deserialize)]
struct ResetForm {
    #[serde(default)]
    token: String,
    password: String,
    password_confirm: String,
}

/// Sets the new password and ends every session, in case someone else
/// had got in.
async fn reset(
    State(state): State<AppState>,
    page: Page,
    jar: CookieJar,
    Form(form): Form<ResetForm>,
) -> Result<Response, AppError> {
    let invalid = |message: String| {
        Ok(page.render_with_status(
            StatusCode::UNPROCESSABLE_ENTITY,
            "reset_password.html",
            context! { token => form.token, error => message },
        ))
    };
    if form.password != form.password_confirm {
        return invalid("The passwords don't match.".into());
    }
    if let Err(e) = accounts::check_password(&form.password) {
        return invalid(format!("The password {e}."));
    }
    let mut tx = state.db.primary().begin().await?;
    let redeemed = account_tokens::redeem(&mut *tx, Purpose::ResetPassword, &form.token)
        .await?
        .ok_or_else(expired_link)?;
    let user = users::by_id(&mut *tx, redeemed.user_id)
        .await?
        .ok_or_else(expired_link)?;
    moekura_db::accounts::set_password(&mut *tx, user.id, &form.password)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    // Following the link proved they read mail at the address.
    if user.email.as_deref() == Some(redeemed.email.as_str()) && user.email_verified_at.is_none() {
        users::set_email(&mut *tx, user.id, Some(&redeemed.email), true)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        finish_signup(&mut tx, &state, &user).await?;
    }
    let ended = sessions::delete_all_for_user(&mut *tx, user.id).await?;
    tx.commit().await?;
    tracing::info!(user_id = user.id, sessions_ended = ended, "password reset");
    Ok((
        flash::set(jar, Flash::PasswordChanged),
        Redirect::to("/login"),
    )
        .into_response())
}

// ---- account settings -----------------------------------------------------

#[derive(Debug, Default, serde::Serialize)]
struct AccountErrors {
    email: Option<String>,
    password: Option<String>,
}

fn render_account(
    page: &Page,
    user: &User,
    errors: &AccountErrors,
    status: StatusCode,
) -> Response {
    page.render_with_status(
        status,
        "account.html",
        context! {
            email => user.email,
            verified => user.email_verified_at.is_some(),
            mail_enabled => mail_enabled(page.state()),
            errors => errors,
        },
    )
}

fn logged_in(page: &Page) -> Result<User, AppError> {
    page.current.user.clone().ok_or(AppError::Unauthorized)
}

async fn account(page: Page) -> Result<Response, AppError> {
    let user = logged_in(&page)?;
    Ok(render_account(
        &page,
        &user,
        &AccountErrors::default(),
        StatusCode::OK,
    ))
}

#[derive(Debug, Deserialize)]
struct EmailChange {
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
}

/// With mail, the new address only replaces the old one once confirmed
/// through a link sent to it; without, it's changed straight away.
async fn change_email(
    page: Page,
    jar: CookieJar,
    info: RequestInfo,
    Form(form): Form<EmailChange>,
) -> Result<Response, AppError> {
    let user = logged_in(&page)?;
    let state = page.state();
    let db = state.db.primary();
    let failed = |message: String| {
        let errors = AccountErrors {
            email: Some(message),
            ..Default::default()
        };
        Ok(render_account(
            &page,
            &user,
            &errors,
            StatusCode::UNPROCESSABLE_ENTITY,
        ))
    };
    // Guessing the password through this form is guessing a login.
    state.rate_limits.check_login(info.ip, &user.name).await?;
    if !moekura_db::accounts::check_password_of(db, user.id, &form.password).await? {
        return failed("Wrong password.".into());
    }
    let email = form.email.trim();
    let unchanged = user
        .email
        .as_deref()
        .is_some_and(|current| current.eq_ignore_ascii_case(email));
    let saved =
        |message| Ok((flash::set(jar, message), Redirect::to("/settings/account")).into_response());

    if email.is_empty() {
        users::set_email(db, user.id, None, false)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        tracing::info!(user_id = user.id, "email address removed");
        return saved(Flash::Saved);
    }
    if let Err(e) = accounts::check_email(email) {
        return failed(format!("The address {e}."));
    }
    if unchanged && (user.email_verified_at.is_some() || !mail_enabled(state)) {
        return saved(Flash::Saved);
    }
    if !unchanged && users::by_email(db, email).await?.is_some() {
        return failed("That address is already in use.".into());
    }
    if mail_enabled(state) {
        state.rate_limits.check_mail(info.ip, email).await?;
        let mut tx = db.begin().await?;
        send_verification(&mut tx, state, &user, email).await?;
        tx.commit().await?;
        return saved(Flash::CheckEmail);
    }
    match users::set_email(db, user.id, Some(email), false).await {
        Ok(()) => {
            tracing::info!(user_id = user.id, "email address changed");
            saved(Flash::Saved)
        }
        Err(InsertError::EmailTaken) => failed("That address is already in use.".into()),
        Err(e) => Err(AppError::Internal(e.to_string())),
    }
}

#[derive(Debug, Deserialize)]
struct PasswordChange {
    #[serde(default)]
    current: String,
    password: String,
    password_confirm: String,
}

/// Changes the password and logs out every other session.
async fn change_password(
    page: Page,
    jar: CookieJar,
    info: RequestInfo,
    Form(form): Form<PasswordChange>,
) -> Result<Response, AppError> {
    let user = logged_in(&page)?;
    let state = page.state();
    let db = state.db.primary();
    let failed = |message: String| {
        let errors = AccountErrors {
            password: Some(message),
            ..Default::default()
        };
        Ok(render_account(
            &page,
            &user,
            &errors,
            StatusCode::UNPROCESSABLE_ENTITY,
        ))
    };
    state.rate_limits.check_login(info.ip, &user.name).await?;
    if !moekura_db::accounts::check_password_of(db, user.id, &form.current).await? {
        return failed("Your current password is wrong.".into());
    }
    if form.password != form.password_confirm {
        return failed("The new passwords don't match.".into());
    }
    let mut tx = db.begin().await?;
    match moekura_db::accounts::set_password(&mut *tx, user.id, &form.password).await {
        Ok(()) => {}
        Err(PasswordChangeError::Invalid(e)) => return failed(format!("The new password {e}.")),
        Err(PasswordChangeError::Db(e)) => return Err(e.into()),
    }
    let ended = match jar.get(SESSION_COOKIE) {
        Some(cookie) => sessions::delete_others(&mut *tx, user.id, cookie.value()).await?,
        None => 0,
    };
    tx.commit().await?;
    tracing::info!(
        user_id = user.id,
        sessions_ended = ended,
        "password changed"
    );
    Ok((
        flash::set(jar, Flash::PasswordChanged),
        Redirect::to("/settings/account"),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use moekura_db::accounts::{self, NewAccount};
    use moekura_db::{roles, settings};
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, test_config, test_state, test_state_with};

    async fn app(pool: &PgPool, mail: bool) -> TestApp {
        let mut config = test_config();
        if mail {
            config.mail.host = "localhost".into();
            config.mail.from = "Moekura <noreply@example.com>".into();
        }
        let state = if mail {
            test_state_with(pool, config).await
        } else {
            test_state(pool).await
        };
        TestApp::new(
            state,
            routes()
                .merge(crate::account::routes())
                .merge(crate::posts::routes()),
        )
    }

    /// The newest queued message: (to, subject, body).
    async fn last_mail(pool: &PgPool) -> Option<(String, String, String)> {
        let payload: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT payload FROM jobs WHERE kind = 'mail.send' ORDER BY id DESC LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .unwrap();
        payload.map(|p| {
            let mail: SendMail = serde_json::from_value(p).unwrap();
            (mail.to, mail.subject, mail.body)
        })
    }

    async fn mail_count(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'mail.send'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The token in the link in `body`.
    fn token(body: &str) -> String {
        let start = body.find("token=").expect("a link with a token") + "token=".len();
        body[start..start + 64].to_owned()
    }

    fn form(fields: &[(&str, &str)]) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish()
    }

    /// A member with a password and email, and a session for them.
    async fn member(pool: &PgPool, name: &str, email: &str) -> (User, String) {
        let role = roles::by_system(pool, SystemRole::Member).await.unwrap();
        let user = accounts::create(
            pool,
            NewAccount {
                name,
                password: "correct horse",
                email: Some(email),
                role_id: role.id,
                status: UserStatus::Active,
            },
        )
        .await
        .unwrap();
        let session = moekura_db::sessions::create(
            pool,
            moekura_db::sessions::NewSession {
                user_id: user.id,
                user_agent: None,
                ip: None,
            },
            moekura_db::sessions::Lifetime {
                idle: Duration::from_secs(3600),
                max: Duration::from_secs(3600),
            },
        )
        .await
        .unwrap();
        (user, session)
    }

    async fn sessions_of(pool: &PgPool, user_id: i64) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn signup(name: &str, email: &str) -> String {
        form(&[
            ("name", name),
            ("email", email),
            ("password", "correct horse"),
            ("password_confirm", "correct horse"),
        ])
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn new_accounts_confirm_their_address(pool: PgPool) {
        settings::set(&pool, "email_verification", json!(true))
            .await
            .unwrap();
        let app = app(&pool, true).await;
        assert!(
            app.get("/register", None)
                .await
                .body
                .contains("type=\"email\" value=\"\" autocomplete=\"email\" required")
        );
        let missing = app
            .post_form("/register", None, &[], &signup("alice", ""))
            .await;
        assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);

        let response = app
            .post_form(
                "/register",
                None,
                &[],
                &signup("alice", "alice@example.com"),
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        assert!(response.session_cookie().is_none(), "not logged in yet");
        let user = users::by_name(&pool, "alice").await.unwrap().unwrap();
        assert_eq!(user.status, UserStatus::Unverified);
        let (to, subject, body) = last_mail(&pool).await.unwrap();
        assert_eq!(to, "alice@example.com");
        assert!(subject.contains("Confirm"), "{subject}");
        assert!(
            body.contains("http://localhost:8080/verify-email?token="),
            "{body}"
        );

        let login = form(&[("name", "alice"), ("password", "correct horse")]);
        let refused = app.post_form("/login", None, &[], &login).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("Confirm your email address first"));
        assert!(refused.body.contains("/verify-email/resend"));

        // Asking again sends a new link, and only the new one works.
        let old = token(&body);
        app.post_form("/verify-email/resend", None, &[], "name=alice")
            .await;
        let new = token(&last_mail(&pool).await.unwrap().2);
        assert_ne!(old, new);
        let stale = app.get(&format!("/verify-email?token={old}"), None).await;
        assert_eq!(stale.status, StatusCode::BAD_REQUEST);

        let confirmed = app.get(&format!("/verify-email?token={new}"), None).await;
        assert_eq!(confirmed.status, StatusCode::SEE_OTHER);
        assert_eq!(confirmed.location.as_deref(), Some("/login"));
        let user = users::by_id(&pool, user.id).await.unwrap().unwrap();
        assert_eq!(user.status, UserStatus::Active);
        assert!(user.email_verified_at.is_some());
        let again = app.get(&format!("/verify-email?token={new}"), None).await;
        assert_eq!(again.status, StatusCode::BAD_REQUEST);
        let login = app.post_form("/login", None, &[], &login).await;
        assert_eq!(login.status, StatusCode::SEE_OTHER);

        // On sites that approve accounts, confirming leads to the queue.
        settings::set(&pool, "registration_mode", json!("approval"))
            .await
            .unwrap();
        let app = super::tests::app(&pool, true).await;
        app.post_form("/register", None, &[], &signup("bob", "bob@example.com"))
            .await;
        let link = token(&last_mail(&pool).await.unwrap().2);
        app.get(&format!("/verify-email?token={link}"), None).await;
        let bob = users::by_name(&pool, "bob").await.unwrap().unwrap();
        assert_eq!(bob.status, UserStatus::Pending);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn without_mail_nothing_needs_it(pool: PgPool) {
        settings::set(&pool, "email_verification", json!(true))
            .await
            .unwrap();
        let app = app(&pool, false).await;
        let response = app
            .post_form("/register", None, &[], &signup("alice", ""))
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let user = users::by_name(&pool, "alice").await.unwrap().unwrap();
        assert_eq!(user.status, UserStatus::Active);
        assert_eq!(
            app.get("/forgot-password", None).await.status,
            StatusCode::NOT_FOUND
        );
        assert!(
            !app.get("/login", None)
                .await
                .body
                .contains("/forgot-password")
        );
        assert_eq!(mail_count(&pool).await, 0);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn forgotten_passwords_are_reset(pool: PgPool) {
        let app = app(&pool, true).await;
        let (alice, _) = member(&pool, "alice", "alice@example.com").await;
        assert!(
            app.get("/login", None)
                .await
                .body
                .contains("/forgot-password")
        );

        // The same answer whether or not an account uses the address.
        let unknown = app
            .post_form("/forgot-password", None, &[], "email=nobody%40example.com")
            .await;
        let known = app
            .post_form("/forgot-password", None, &[], "email=Alice%40example.com")
            .await;
        assert_eq!(unknown.status, StatusCode::OK);
        assert_eq!(
            unknown.body.replace("nobody@example.com", "X"),
            known.body.replace("Alice@example.com", "X")
        );
        assert_eq!(mail_count(&pool).await, 1);
        let (to, _, body) = last_mail(&pool).await.unwrap();
        assert_eq!(to, "alice@example.com");
        let link = token(&body);

        let reset = format!("/reset-password?token={link}");
        assert_eq!(app.get(&reset, None).await.status, StatusCode::OK);
        let bad = app.get("/reset-password?token=nope", None).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        let mismatch = app
            .post_form(
                "/reset-password",
                None,
                &[],
                &form(&[
                    ("token", &link),
                    ("password", "new horse!"),
                    ("password_confirm", "other"),
                ]),
            )
            .await;
        assert_eq!(mismatch.status, StatusCode::UNPROCESSABLE_ENTITY);

        let fields = form(&[
            ("token", &link),
            ("password", "battery staple"),
            ("password_confirm", "battery staple"),
        ]);
        let done = app.post_form("/reset-password", None, &[], &fields).await;
        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.body);
        assert_eq!(done.location.as_deref(), Some("/login"));
        assert_eq!(
            sessions_of(&pool, alice.id).await,
            0,
            "logged out everywhere"
        );
        accounts::authenticate(&pool, "alice", "battery staple")
            .await
            .unwrap();
        let alice = users::by_id(&pool, alice.id).await.unwrap().unwrap();
        assert!(
            alice.email_verified_at.is_some(),
            "the link proved the address"
        );
        let reused = app.post_form("/reset-password", None, &[], &fields).await;
        assert_eq!(reused.status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn changing_the_password_ends_other_sessions(pool: PgPool) {
        let app = app(&pool, false).await;
        let (alice, session) = member(&pool, "alice", "alice@example.com").await;
        moekura_db::sessions::create(
            &pool,
            moekura_db::sessions::NewSession {
                user_id: alice.id,
                user_agent: None,
                ip: None,
            },
            moekura_db::sessions::Lifetime {
                idle: Duration::from_secs(3600),
                max: Duration::from_secs(3600),
            },
        )
        .await
        .unwrap();
        assert_eq!(sessions_of(&pool, alice.id).await, 2);
        assert_eq!(
            app.get("/settings/account", None).await.location.as_deref(),
            Some("/login?next=%2Fsettings%2Faccount")
        );
        assert!(
            app.get("/settings/account", Some(&session))
                .await
                .body
                .contains("alice@example.com")
        );

        let change = |current: &str| {
            form(&[
                ("current", current),
                ("password", "battery staple"),
                ("password_confirm", "battery staple"),
            ])
        };
        let wrong = app
            .post_form(
                "/settings/account/password",
                Some(&session),
                &[],
                &change("nope"),
            )
            .await;
        assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(wrong.body.contains("current password is wrong"));
        let done = app
            .post_form(
                "/settings/account/password",
                Some(&session),
                &[],
                &change("correct horse"),
            )
            .await;
        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.body);
        assert_eq!(sessions_of(&pool, alice.id).await, 1, "this session stays");
        assert_eq!(
            app.get("/settings/account", Some(&session)).await.status,
            StatusCode::OK
        );
        accounts::authenticate(&pool, "alice", "battery staple")
            .await
            .unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn changing_the_address(pool: PgPool) {
        // Without mail, straight away.
        let plain = app(&pool, false).await;
        let (alice, session) = member(&pool, "alice", "alice@example.com").await;
        member(&pool, "bob", "bob@example.com").await;
        let change =
            |email: &str, password: &str| form(&[("email", email), ("password", password)]);
        let wrong = plain
            .post_form(
                "/settings/account/email",
                Some(&session),
                &[],
                &change("a@example.com", "nope"),
            )
            .await;
        assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
        let taken = plain
            .post_form(
                "/settings/account/email",
                Some(&session),
                &[],
                &change("BOB@example.com", "correct horse"),
            )
            .await;
        assert!(taken.body.contains("already in use"), "{}", taken.body);
        let done = plain
            .post_form(
                "/settings/account/email",
                Some(&session),
                &[],
                &change("a@example.com", "correct horse"),
            )
            .await;
        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.body);
        let user = users::by_id(&pool, alice.id).await.unwrap().unwrap();
        assert_eq!(user.email.as_deref(), Some("a@example.com"));

        // With mail, once confirmed from the new address.
        let mailing = app(&pool, true).await;
        mailing
            .post_form(
                "/settings/account/email",
                Some(&session),
                &[],
                &change("new@example.com", "correct horse"),
            )
            .await;
        let user = users::by_id(&pool, alice.id).await.unwrap().unwrap();
        assert_eq!(user.email.as_deref(), Some("a@example.com"), "not yet");
        let (to, _, body) = last_mail(&pool).await.unwrap();
        assert_eq!(to, "new@example.com");
        let confirmed = mailing
            .get(
                &format!("/verify-email?token={}", token(&body)),
                Some(&session),
            )
            .await;
        assert_eq!(confirmed.location.as_deref(), Some("/settings/account"));
        let user = users::by_id(&pool, alice.id).await.unwrap().unwrap();
        assert_eq!(user.email.as_deref(), Some("new@example.com"));
        assert!(user.email_verified_at.is_some());

        // Removing it.
        mailing
            .post_form(
                "/settings/account/email",
                Some(&session),
                &[],
                &change("", "correct horse"),
            )
            .await;
        let user = users::by_id(&pool, alice.id).await.unwrap().unwrap();
        assert_eq!(user.email, None);
    }
}
