//! Registration, login and logout pages.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::context;
use moekura_core::accounts::{NAME_MAX_LEN, NAME_MIN_LEN};
use moekura_core::permissions::SystemRole;
use moekura_core::settings::RegistrationMode;
use moekura_db::accounts::{self, AuthError, CreateError, NewAccount};
use moekura_db::users::{self, UserStatus};
use moekura_db::{invites, user_ips};
use serde::Deserialize;

use crate::AppState;
use crate::auth::{self, RequestInfo};
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/register", get(register_form).post(register))
        .route("/login", get(login_form).post(login))
        .route("/logout", post(logout))
}

#[derive(Debug, Default, Deserialize)]
struct NextQuery {
    next: Option<String>,
    /// An invite code, from a link that comes with it.
    #[serde(default)]
    invite: String,
}

/// Where to send the user after logging in: a local path only, so the
/// parameter can't bounce people to another site.
pub(crate) fn safe_next(next: Option<&str>) -> &str {
    match next {
        Some(path)
            if path.starts_with('/')
                && !path.starts_with("//")
                && !path.starts_with("/\\")
                && !path.chars().any(char::is_control)
                && !path.starts_with("/login")
                && !path.starts_with("/register") =>
        {
            path
        }
        _ => "/",
    }
}

// ---- registration ---------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct RegisterForm {
    name: String,
    #[serde(default)]
    email: String,
    password: String,
    password_confirm: String,
    #[serde(default)]
    invite: String,
    next: Option<String>,
    /// The captcha widget's token, under the name its service gives it.
    #[serde(default, alias = "cf-turnstile-response", alias = "h-captcha-response")]
    captcha: String,
}

/// Errors shown next to the form fields.
#[derive(Debug, Default, serde::Serialize)]
struct RegisterErrors {
    name: Option<String>,
    email: Option<String>,
    password: Option<String>,
    invite: Option<String>,
    captcha: Option<String>,
}

/// Why an address at a refused domain can't be used.
pub(crate) const DOMAIN_REFUSED: &str = "Addresses at that domain can't be used here.";

fn registration_mode(state: &AppState) -> RegistrationMode {
    state.site.get().settings.registration_mode
}

async fn register_form(page: Page, Query(query): Query<NextQuery>) -> Result<Response, AppError> {
    if page.current.is_logged_in() {
        return Ok(Redirect::to("/").into_response());
    }
    let mode = registration_mode(page.state());
    if mode == RegistrationMode::Closed {
        return Err(AppError::Forbidden);
    }
    let form = RegisterForm {
        next: query.next,
        invite: query.invite,
        ..Default::default()
    };
    Ok(render_register(
        &page,
        mode,
        &form,
        &RegisterErrors::default(),
        StatusCode::OK,
    ))
}

fn render_register(
    page: &Page,
    mode: RegistrationMode,
    form: &RegisterForm,
    errors: &RegisterErrors,
    status: StatusCode,
) -> Response {
    page.render_with_status(
        status,
        "register.html",
        context! {
            form => context! { name => form.name, email => form.email, invite => form.invite },
            next => form.next,
            errors => errors,
            needs_invite => mode == RegistrationMode::Invite,
            needs_approval => mode == RegistrationMode::Approval,
            email_required => crate::email::verification_required(page.state()),
            mail_enabled => crate::email::mail_enabled(page.state()),
            captcha => crate::captcha::for_sign_up(page.state()).map(|c| c.widget()),
        },
    )
}

async fn register(
    State(state): State<AppState>,
    page: Page,
    jar: CookieJar,
    info: RequestInfo,
    Form(form): Form<RegisterForm>,
) -> Result<Response, AppError> {
    if page.current.is_logged_in() {
        return Ok(Redirect::to("/").into_response());
    }
    let mode = registration_mode(&state);
    if mode == RegistrationMode::Closed {
        return Err(AppError::Forbidden);
    }
    state
        .rate_limits
        .check_register(info.ip)
        .await
        .inspect_err(|_| {
            tracing::warn!(ip = ?info.ip, "registration rate limited");
        })?;
    let invalid = |errors: RegisterErrors| {
        Ok(render_register(
            &page,
            mode,
            &form,
            &errors,
            StatusCode::UNPROCESSABLE_ENTITY,
        ))
    };

    if let Some(captcha) = crate::captcha::for_sign_up(&state)
        && let Err(message) = captcha.check(&form.captcha, info.ip).await
    {
        return invalid(RegisterErrors {
            captcha: Some(message),
            ..Default::default()
        });
    }
    if form.password != form.password_confirm {
        return invalid(RegisterErrors {
            password: Some("The passwords don't match.".into()),
            ..Default::default()
        });
    }
    let verify = crate::email::verification_required(&state);
    let email = form.email.trim();
    if verify && email.is_empty() {
        return invalid(RegisterErrors {
            email: Some("An email address is required, to confirm your account.".into()),
            ..Default::default()
        });
    }
    // A bare address first: the domain list reads what follows the `@`.
    if !email.is_empty()
        && let Err(e) = moekura_core::accounts::check_email(email)
    {
        return invalid(RegisterErrors {
            email: Some(format!("The address {e}.")),
            ..Default::default()
        });
    }
    if !email.is_empty() && !state.site.get().settings.email_domains.allows(email) {
        return invalid(RegisterErrors {
            email: Some(DOMAIN_REFUSED.into()),
            ..Default::default()
        });
    }
    if mode == RegistrationMode::Invite && form.invite.trim().is_empty() {
        return invalid(RegisterErrors {
            invite: Some("An invite code is required.".into()),
            ..Default::default()
        });
    }
    // The tagger's, even before it has made its account.
    if state.config.tagger.reserves(&form.name) {
        return invalid(RegisterErrors {
            name: Some("That name is taken.".into()),
            ..Default::default()
        });
    }

    let site = state.site.get();
    let member = site
        .system_role(SystemRole::Member)
        .ok_or_else(|| AppError::Internal("the Member role is missing".into()))?;
    // Approval, if needed, comes after the address is confirmed.
    let status = match mode {
        _ if verify => UserStatus::Unverified,
        RegistrationMode::Approval => UserStatus::Pending,
        _ => UserStatus::Active,
    };
    let mailing = !email.is_empty() && crate::email::mail_enabled(&state);

    // One transaction, so a failed signup doesn't use up the invite.
    let mut tx = state.db.primary().begin().await?;
    // With mail, an address another account has gets the same answer as a
    // free one, and its owner is told instead, so signing up can't be used
    // to find out who has an account. The new account goes without it.
    // Without mail nobody could be told, and the address is the account's
    // at once, so a taken one is refused as before.
    let taken = match mailing {
        true => users::by_email(&mut *tx, email).await?,
        false => None,
    };
    // An address waiting to be confirmed for an account that can't log in
    // until then is kept on it; otherwise, with mail, it becomes the
    // account's once the link sent to it is followed, as when changing it.
    let keep_email = !mailing || (verify && taken.is_none());
    let invite = match mode {
        RegistrationMode::Invite => match invites::redeem(&mut *tx, &form.invite).await? {
            Some(id) => Some(id),
            None => {
                return invalid(RegisterErrors {
                    invite: Some("That invite code is invalid, expired or already used.".into()),
                    ..Default::default()
                });
            }
        },
        _ => None,
    };
    let account = NewAccount {
        name: form.name.trim(),
        password: &form.password,
        email: keep_email.then_some(email),
        role_id: member.id,
        status,
    };
    let user = match accounts::create(&mut *tx, account).await {
        Ok(user) => user,
        Err(error) => {
            let mut errors = RegisterErrors::default();
            match error {
                CreateError::InvalidName(e) => errors.name = Some(format!("The name {e}.")),
                CreateError::NameTaken => errors.name = Some("That name is taken.".into()),
                CreateError::InvalidPassword(e) => {
                    errors.password = Some(format!("The password {e}."))
                }
                CreateError::InvalidEmail(e) => errors.email = Some(format!("The address {e}.")),
                CreateError::EmailTaken => {
                    errors.email = Some("That address is already in use.".into())
                }
                CreateError::Db(e) => return Err(e.into()),
            }
            return invalid(errors);
        }
    };
    if let Some(invite) = invite {
        invites::record_use(&mut *tx, invite, user.id).await?;
    }
    if mailing {
        // Only now that a message goes out, so a form sent back for a
        // taken name doesn't use up the address's mail. Refused, the
        // account and the invite are rolled back.
        state.rate_limits.check_mail(info.ip, email).await?;
        match &taken {
            Some(owner) => crate::email::send_address_in_use(&mut tx, &state, owner).await?,
            None => crate::email::send_verification(&mut tx, &state, &user, email).await?,
        }
    }
    tx.commit().await?;
    tracing::info!(user_id = user.id, name = %user.name, ?status, "account registered");
    crate::webhooks::emit_user(&state, &user).await;

    if status == UserStatus::Unverified {
        let jar = flash::set(jar, Flash::CheckEmail);
        return Ok((jar, Redirect::to("/")).into_response());
    }
    if status == UserStatus::Pending {
        let jar = flash::set(jar, Flash::AwaitingApproval);
        return Ok((jar, Redirect::to("/")).into_response());
    }
    let jar = auth::log_in(&state, jar, &info, &user).await?;
    let jar = flash::set(jar, Flash::Registered);
    Ok((jar, Redirect::to(safe_next(form.next.as_deref()))).into_response())
}

// ---- login / logout -------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct LoginForm {
    name: String,
    password: String,
    next: Option<String>,
    /// The captcha widget's token, once one is asked for (see [`login`]).
    #[serde(default, alias = "cf-turnstile-response", alias = "h-captcha-response")]
    captcha: String,
}

/// A captcha the login form asks for, and what's wrong with the last
/// answer, if anything.
type CaptchaPrompt<'a> = (&'a crate::captcha::Captcha, Option<&'a str>);

/// At most as much of a typed `name` as an account name can be, for logs
/// and for showing it again.
fn clipped(name: &str) -> &str {
    &name[..name.floor_char_boundary(NAME_MAX_LEN)]
}

/// Whether some account could be called `name`: account names are 2 to
/// 32 ASCII letters, digits, `_`, `.` and `-` (see `UserName`, and the
/// check on `users.name`).
fn could_be_account_name(name: &str) -> bool {
    (NAME_MIN_LEN..=NAME_MAX_LEN).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

async fn login_form(page: Page, Query(query): Query<NextQuery>) -> Response {
    if page.current.is_logged_in() {
        return Redirect::to(safe_next(query.next.as_deref())).into_response();
    }
    render_login(&page, "", query.next.as_deref(), None, None, StatusCode::OK)
}

fn render_login(
    page: &Page,
    name: &str,
    next: Option<&str>,
    error: Option<&AuthError>,
    captcha: Option<CaptchaPrompt>,
    status: StatusCode,
) -> Response {
    let message = error.map(|error| match error {
        AuthError::Pending => "Your account is still waiting for approval.",
        AuthError::Unverified => "Confirm your email address first, with the link we sent you.",
        AuthError::Deactivated => "This account has been deactivated.",
        _ => "Wrong name or password.",
    });
    page.render_with_status(
        status,
        "login.html",
        context! {
            name => name,
            next => next,
            error => message,
            unverified => matches!(error, Some(AuthError::Unverified)),
            captcha => captcha.map(|(captcha, _)| captcha.widget()),
            captcha_error => captcha.and_then(|(_, error)| error),
            mail_enabled => crate::email::mail_enabled(page.state()),
            sso_label => page.state().oidc.as_ref().map(|o| o.button_label().to_owned()),
        },
    )
}

async fn login(
    State(state): State<AppState>,
    page: Page,
    jar: CookieJar,
    info: RequestInfo,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let name = form.name.trim();
    // No account has a name like this, so there's nothing to count, look
    // up or log in full. Refused before the limits also because the
    // database would take some other spellings (`İ` for `i`) for a real
    // account's name.
    if !could_be_account_name(name) {
        tracing::info!(
            name = clipped(name),
            reason = "no account could have this name",
            "login failed"
        );
        let error = AuthError::InvalidCredentials;
        return Ok(render_login(
            &page,
            clipped(name),
            form.next.as_deref(),
            Some(&error),
            None,
            StatusCode::UNPROCESSABLE_ENTITY,
        ));
    }
    let limited = || tracing::warn!(ip = ?info.ip, name = clipped(name), "login rate limited");
    let limits = &state.rate_limits;
    limits
        .check_login(info.ip, name)
        .await
        .inspect_err(|_| limited())?;
    // Past the limit for the account from all networks together, someone
    // is guessing from many. Then an attempt gets through with a solved
    // captcha, if the site has a captcha service, and the owner also gets
    // in with the right password from a network the account has used.
    // Everyone else is refused alike, after the same work, so a refusal
    // tells nothing about the password or the network.
    let ceiling = limits.check_login_ceiling(name).await;
    let captcha = match (&ceiling, state.captcha.as_deref()) {
        (Err(_), Some(captcha)) => Some((captcha, captcha.check(&form.captcha, info.ip).await)),
        _ => None,
    };
    let solved = matches!(captcha, Some((_, Ok(()))));
    let used_network = match (&ceiling, info.ip) {
        (Err(_), Some(ip)) if !solved => {
            let network = crate::rate_limit::ip_bucket_net(ip);
            user_ips::name_used(state.db.primary(), name, network).await?
        }
        _ => false,
    };
    let result = accounts::authenticate(state.db.primary(), name, &form.password).await;
    if let Err(refused) = ceiling
        && !solved
        && !(used_network && result.is_ok())
    {
        if let Err(AuthError::Db(error)) = result {
            return Err(error.into());
        }
        limited();
        return match &captcha {
            Some((captcha, Err(message))) => Ok(render_login(
                &page,
                name,
                form.next.as_deref(),
                None,
                Some((*captcha, Some(message.as_str()))),
                StatusCode::TOO_MANY_REQUESTS,
            )),
            _ => Err(refused),
        };
    }
    let user = match result {
        Ok(user) => user,
        Err(AuthError::Db(error)) => return Err(error.into()),
        Err(error) => {
            tracing::info!(name = clipped(name), reason = %error, "login failed");
            let status = StatusCode::UNPROCESSABLE_ENTITY;
            // Still past the limit: the next attempt needs one too.
            let captcha = captcha.map(|(captcha, _)| (captcha, None));
            return Ok(render_login(
                &page,
                name,
                form.next.as_deref(),
                Some(&error),
                captcha,
                status,
            ));
        }
    };
    let next = safe_next(form.next.as_deref());
    let jar = match crate::two_factor::challenge_if_enabled(&state, jar, &user, Some(next)).await? {
        Ok(jar) => jar,
        Err(code_form) => return Ok(code_form),
    };
    let jar = auth::log_in(&state, jar, &info, &user).await?;
    let jar = flash::set(jar, Flash::LoggedIn);
    Ok((jar, Redirect::to(next)).into_response())
}

async fn logout(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let jar = auth::log_out(&state, jar).await?;
    let jar = flash::set(jar, Flash::LoggedOut);
    Ok((jar, Redirect::to("/")).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_db::users::{self, UserStatus};
    use moekura_db::{invites, settings};
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(
            test_state(pool).await,
            routes().merge(crate::posts::routes()),
        )
    }

    async fn set_mode(pool: &PgPool, mode: &str) {
        settings::set(pool, "registration_mode", json!(mode))
            .await
            .unwrap();
    }

    fn form(fields: &[(&str, &str)]) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish()
    }

    fn signup(name: &str) -> String {
        form(&[
            ("name", name),
            ("password", "correct horse"),
            ("password_confirm", "correct horse"),
        ])
    }

    #[test]
    fn next_must_be_a_local_path() {
        assert_eq!(safe_next(Some("/posts?page=2")), "/posts?page=2");
        for unsafe_next in [
            "https://evil.example",
            "//evil.example",
            "/\\evil.example",
            "evil",
            "/login",
        ] {
            assert_eq!(safe_next(Some(unsafe_next)), "/", "{unsafe_next}");
        }
        assert_eq!(safe_next(None), "/");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn open_registration_logs_the_new_user_in(pool: PgPool) {
        let app = app(&pool).await;
        let form = format!("{}&next=%2Fsomewhere", signup("alice"));
        let response = app.post_form("/register", None, &[], &form).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        assert_eq!(response.location.as_deref(), Some("/somewhere"));

        let session = response.session_cookie().expect("logged in");
        let home = app.get("/", Some(&session)).await;
        assert!(home.body.contains("alice"), "{}", home.body);

        let user = users::by_name(&pool, "alice").await.unwrap().unwrap();
        assert_eq!(user.status, UserStatus::Active);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn validation_errors_keep_input_but_not_passwords(pool: PgPool) {
        let app = app(&pool).await;
        let bad = form(&[
            ("name", "alice"),
            ("email", "alice@example.com"),
            ("password", "correct horse"),
            ("password_confirm", "different horse"),
        ]);
        let response = app.post_form("/register", None, &[], &bad).await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            response.body.contains("The passwords don&#x27;t match."),
            "{}",
            response.body
        );
        assert!(response.body.contains("value=\"alice\""));
        assert!(response.body.contains("value=\"alice@example.com\""));
        assert!(!response.body.contains("correct horse"));

        let taken = app
            .post_form("/register", None, &[], &signup("alice"))
            .await;
        assert_eq!(taken.status, StatusCode::SEE_OTHER);
        let again = app
            .post_form("/register", None, &[], &signup("ALICE"))
            .await;
        assert!(again.body.contains("That name is taken."), "{}", again.body);
        let reserved = app
            .post_form("/register", None, &[], &signup("admin"))
            .await;
        assert!(
            reserved.body.contains("The name is reserved."),
            "{}",
            reserved.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn without_mail_a_taken_address_is_refused(pool: PgPool) {
        // Nobody could be told of it, and the address is the account's at
        // once, so this is the only way to say so.
        let app = app(&pool).await;
        let with_email = |name: &str, email: &str| {
            form(&[
                ("name", name),
                ("email", email),
                ("password", "correct horse"),
                ("password_confirm", "correct horse"),
            ])
        };
        let first = app
            .post_form(
                "/register",
                None,
                &[],
                &with_email("alice", "a@example.com"),
            )
            .await;
        assert_eq!(first.status, StatusCode::SEE_OTHER);
        let user = users::by_name(&pool, "alice").await.unwrap().unwrap();
        assert_eq!(user.email.as_deref(), Some("a@example.com"));
        let taken = app
            .post_form("/register", None, &[], &with_email("bob", "A@example.com"))
            .await;
        assert_eq!(taken.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(taken.body.contains("already in use"), "{}", taken.body);
        let name_addr = app
            .post_form(
                "/register",
                None,
                &[],
                &with_email("bob", "Bob <bob@example.com>"),
            )
            .await;
        assert!(
            name_addr.body.contains("not a valid email address"),
            "{}",
            name_addr.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn nobody_signs_up_as_the_tagger(pool: PgPool) {
        let app = app(&pool).await;
        let response = app
            .post_form("/register", None, &[], &signup("Tagger"))
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(response.body.contains("That name is taken."));
        assert!(users::by_name(&pool, "tagger").await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn approval_mode_creates_pending_accounts(pool: PgPool) {
        set_mode(&pool, "approval").await;
        let app = app(&pool).await;
        let response = app
            .post_form("/register", None, &[], &signup("alice"))
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        assert!(
            response.session_cookie().is_none(),
            "pending users are not logged in"
        );
        let user = users::by_name(&pool, "alice").await.unwrap().unwrap();
        assert_eq!(user.status, UserStatus::Pending);

        let login = form(&[("name", "alice"), ("password", "correct horse")]);
        let response = app.post_form("/login", None, &[], &login).await;
        assert!(
            response.body.contains("still waiting for approval"),
            "{}",
            response.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn invite_mode_requires_a_working_code(pool: PgPool) {
        set_mode(&pool, "invite").await;
        let code = invites::create(
            &pool,
            invites::NewInvite {
                created_by: None,
                max_uses: 1,
                expires_in: None,
                note: String::new(),
            },
        )
        .await
        .unwrap();
        let app = app(&pool).await;

        let page = app.get("/register", None).await;
        assert!(page.body.contains("name=\"invite\""));

        let missing = app
            .post_form("/register", None, &[], &signup("alice"))
            .await;
        assert!(missing.body.contains("An invite code is required."));

        // A failed signup must not use up the code...
        sqlx::query("INSERT INTO users (name, role_id) SELECT 'taken', id FROM roles WHERE system_key = 'member'")
            .execute(&pool)
            .await
            .unwrap();
        let clash = format!("{}&invite={code}", signup("taken"));
        let response = app.post_form("/register", None, &[], &clash).await;
        assert!(
            response.body.contains("That name is taken."),
            "{}",
            response.body
        );

        // ...so it still works once, and only once.
        let ok = format!("{}&invite={code}", signup("alice"));
        assert_eq!(
            app.post_form("/register", None, &[], &ok).await.status,
            StatusCode::SEE_OTHER
        );
        let reuse = format!("{}&invite={code}", signup("bob"));
        let response = app.post_form("/register", None, &[], &reuse).await;
        assert!(
            response.body.contains("invalid, expired or already used"),
            "{}",
            response.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn closed_mode_refuses_registration(pool: PgPool) {
        set_mode(&pool, "closed").await;
        let app = app(&pool).await;
        assert_eq!(
            app.get("/register", None).await.status,
            StatusCode::FORBIDDEN
        );
        let response = app
            .post_form("/register", None, &[], &signup("alice"))
            .await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
        assert!(users::by_name(&pool, "alice").await.unwrap().is_none());
        // And the header stops offering it.
        assert!(!app.get("/", None).await.body.contains("href=\"/register\""));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn login_and_logout(pool: PgPool) {
        let app = app(&pool).await;
        app.post_form("/register", None, &[], &signup("alice"))
            .await;

        let wrong = form(&[("name", "alice"), ("password", "wrong horse")]);
        let response = app.post_form("/login", None, &[], &wrong).await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(response.body.contains("Wrong name or password."));
        assert!(response.body.contains("value=\"alice\""));

        let right = form(&[
            ("name", "Alice"),
            ("password", "correct horse"),
            ("next", "https://evil.example"),
        ]);
        let response = app.post_form("/login", None, &[], &right).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        assert_eq!(response.location.as_deref(), Some("/"));
        let session = response.session_cookie().unwrap();

        // Logged-in users are sent away from the login page.
        let response = app.get("/login?next=%2Ffoo", Some(&session)).await;
        assert_eq!(response.location.as_deref(), Some("/foo"));

        let response = app.post("/logout", Some(&session), &[]).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        assert!(
            app.get("/", Some(&session))
                .await
                .body
                .contains("href=\"/login\"")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn login_page_keeps_next_for_registering(pool: PgPool) {
        let app = app(&pool).await;
        let response = app.get("/login?next=%2Fupload%3Fa%3D1", None).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        // urlencode keeps `/`, which HTML escaping writes as `&#x2f;`.
        assert!(
            response
                .body
                .contains("href=\"/register?next=&#x2f;upload%3Fa%3D1\""),
            "{}",
            response.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn repeated_login_failures_are_rate_limited(pool: PgPool) {
        let app = app(&pool).await;
        let wrong = form(&[("name", "alice"), ("password", "wrong horse")]);
        for _ in 0..5 {
            let response = app.post_form("/login", None, &[], &wrong).await;
            assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        }
        let limited = app.post_form("/login", None, &[], &wrong).await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            limited.body.contains("Too many attempts"),
            "{}",
            limited.body
        );
        assert!(limited.retry_after.is_some_and(|s| s >= 1));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn impossibly_long_names_are_refused_uncounted(pool: PgPool) {
        let app = app(&pool).await;
        let long = "a".repeat(100_000);
        let attempt = form(&[("name", &long), ("password", "wrong horse")]);
        for _ in 0..10 {
            let response = app.post_form("/login", None, &[], &attempt).await;
            assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
            assert!(response.body.contains("Wrong name or password."));
            // Shown again only as far as a name can go.
            assert!(
                response
                    .body
                    .contains(&format!("value=\"{}\"", &long[..32]))
            );
            assert!(!response.body.contains(&long[..33]));
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn names_no_account_could_have_are_refused_uncounted(pool: PgPool) {
        let app = app(&pool).await;
        app.post_form("/register", None, &[], &signup("alice"))
            .await;
        // The database may take these for alice, but no account can be
        // called them.
        for name in ["al\u{130}ce", "ALİCE", "alice\u{0}", "a"] {
            let attempt = form(&[("name", name), ("password", "wrong horse")]);
            for _ in 0..6 {
                let response = app.post_form("/login", None, &[], &attempt).await;
                assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{name}");
                assert!(response.body.contains("Wrong name or password."));
            }
        }
        let right = form(&[("name", "alice"), ("password", "correct horse")]);
        let response = app.post_form("/login", None, &[], &right).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn guessing_from_one_network_does_not_lock_the_owner_out(pool: PgPool) {
        let mut state = test_state(&pool).await;
        let mut config = (*state.config).clone();
        config.server.trusted_proxies = vec!["10.0.0.0/8".parse().unwrap()];
        state.config = std::sync::Arc::new(config);
        let proxy = "10.0.0.2:40000".parse().unwrap();
        let app = TestApp::with_peer(state, routes(), proxy);
        app.post_form(
            "/register",
            None,
            &[("x-forwarded-for", "198.51.100.1")],
            &signup("alice"),
        )
        .await;

        let guesser = [("x-forwarded-for", "203.0.113.9")];
        let wrong = form(&[("name", "alice"), ("password", "wrong horse")]);
        for _ in 0..5 {
            let response = app.post_form("/login", None, &guesser, &wrong).await;
            assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        }
        let limited = app.post_form("/login", None, &guesser, &wrong).await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);

        let owner = [("x-forwarded-for", "198.51.100.1")];
        let right = form(&[("name", "alice"), ("password", "correct horse")]);
        let response = app.post_form("/login", None, &owner, &right).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn guessing_from_many_networks_lets_the_owner_in_from_theirs(pool: PgPool) {
        let mut state = test_state(&pool).await;
        let mut config = (*state.config).clone();
        config.server.trusted_proxies = vec!["10.0.0.0/8".parse().unwrap()];
        state.config = std::sync::Arc::new(config);
        let limits = state.rate_limits.clone();
        let proxy = "10.0.0.2:40000".parse().unwrap();
        let app = TestApp::with_peer(state, routes(), proxy);
        let response = app
            .post_form(
                "/register",
                None,
                &[("x-forwarded-for", "2001:db8:1:2::1")],
                &signup("alice"),
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);

        // Each attempt follows guesses from enough networks to use up the
        // account's allowance from all of them (used up right before, as a
        // password check takes long enough in tests for it to refill).
        let login = async |client: &str, password: &str| {
            while limits.check_login_ceiling("alice").await.is_ok() {}
            let attempt = form(&[("name", "alice"), ("password", password)]);
            let from = [("x-forwarded-for", client)];
            app.post_form("/login", None, &from, &attempt).await.status
        };
        // Other networks are refused, with the right password too.
        for password in ["wrong horse", "correct horse"] {
            let status = login("203.0.113.7", password).await;
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{password}");
        }
        // From the /64 the account used, a wrong password is refused the
        // same way, and the right one gets in.
        let status = login("2001:db8:1:2::abcd", "wrong horse").await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        let status = login("2001:db8:1:2::abcd", "correct horse").await;
        assert_eq!(status, StatusCode::SEE_OTHER);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn guessing_from_many_networks_lets_the_owner_in_with_a_captcha(pool: PgPool) {
        let mut config = crate::test_support::test_config();
        config.server.trusted_proxies = vec!["10.0.0.0/8".parse().unwrap()];
        config.auth.captcha = Some(crate::captcha::test_service::start().await);
        let state = crate::test_support::test_state_with(&pool, config).await;
        let limits = state.rate_limits.clone();
        let proxy = "10.0.0.2:40000".parse().unwrap();
        let app = TestApp::with_peer(state, routes(), proxy);
        let from = [("x-forwarded-for", "198.51.100.1")];
        let response = app
            .post_form("/register", None, &from, &signup("alice"))
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        // Not asked for while the account's allowance lasts.
        let page = app.get("/login", None).await;
        assert!(!page.body.contains("cf-turnstile"), "{}", page.body);
        let right = form(&[("name", "alice"), ("password", "correct horse")]);
        let response = app.post_form("/login", None, &from, &right).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);

        // From a network the account never used, once guesses from many
        // have used up its allowance.
        let login = async |password: &str, token: &str| {
            while limits.check_login_ceiling("alice").await.is_ok() {}
            let attempt = form(&[
                ("name", "alice"),
                ("password", password),
                ("cf-turnstile-response", token),
            ]);
            let from = [("x-forwarded-for", "203.0.113.7")];
            app.post_form("/login", None, &from, &attempt).await
        };
        for (password, token) in [("correct horse", ""), ("correct horse", "bad")] {
            let refused = login(password, token).await;
            assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS, "{token}");
            assert!(
                refused
                    .body
                    .contains("class=\"cf-turnstile\" data-sitekey=\"site-key\""),
                "{}",
                refused.body
            );
            assert!(refused.body.contains("captcha"), "{}", refused.body);
        }
        // Solved, the attempt is checked as usual.
        let wrong = login("wrong horse", "good").await;
        assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(wrong.body.contains("Wrong name or password."));
        assert!(wrong.body.contains("cf-turnstile"), "{}", wrong.body);
        let right = login("correct horse", "good").await;
        assert_eq!(right.status, StatusCode::SEE_OTHER, "{}", right.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn registration_is_limited_per_client_ip_behind_a_proxy(pool: PgPool) {
        let mut state = test_state(&pool).await;
        let mut config = (*state.config).clone();
        config.server.trusted_proxies = vec!["10.0.0.0/8".parse().unwrap()];
        state.config = std::sync::Arc::new(config);
        let proxy = "10.0.0.2:40000".parse().unwrap();
        let app = TestApp::with_peer(state, routes(), proxy);

        let from = |client: &'static str| [("x-forwarded-for", client)];
        for i in 0..5 {
            let body = signup(&format!("user{i}"));
            let response = app
                .post_form("/register", None, &from("198.51.100.1"), &body)
                .await;
            assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        }
        let limited = app
            .post_form("/register", None, &from("198.51.100.1"), &signup("user5"))
            .await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        // A different client behind the same proxy is not affected.
        let other = app
            .post_form("/register", None, &from("198.51.100.2"), &signup("user6"))
            .await;
        assert_eq!(other.status, StatusCode::SEE_OTHER);

        // Sessions record the real client address.
        let ip: String = sqlx::query_scalar("SELECT host(ip) FROM sessions ORDER BY id LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(ip, "198.51.100.1");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn flash_messages_show_once(pool: PgPool) {
        let app = app(&pool).await;
        let response = app
            .post_form("/register", None, &[], &signup("alice"))
            .await;
        let flash_cookie = response
            .set_cookie
            .iter()
            .find(|c| c.starts_with("moekura_flash="))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let page = app.get_with_cookie("/", &flash_cookie).await;
        assert!(
            page.body.contains("Your account is ready."),
            "{}",
            page.body
        );
        assert!(
            page.set_cookie
                .iter()
                .any(|c| c.starts_with("moekura_flash=;"))
        );
    }
}
