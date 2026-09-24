//! Registration, login and logout pages.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::context;
use moekura_core::permissions::SystemRole;
use moekura_core::settings::RegistrationMode;
use moekura_db::accounts::{self, AuthError, CreateError, NewAccount};
use moekura_db::invites;
use moekura_db::users::UserStatus;
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
}

/// Where to send the user after logging in: a local path only, so the
/// parameter can't bounce people to another site.
fn safe_next(next: Option<&str>) -> &str {
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
}

/// Errors shown next to the form fields.
#[derive(Debug, Default, serde::Serialize)]
struct RegisterErrors {
    name: Option<String>,
    email: Option<String>,
    password: Option<String>,
    invite: Option<String>,
}

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

    if form.password != form.password_confirm {
        return invalid(RegisterErrors {
            password: Some("The passwords don't match.".into()),
            ..Default::default()
        });
    }
    if mode == RegistrationMode::Invite && form.invite.trim().is_empty() {
        return invalid(RegisterErrors {
            invite: Some("An invite code is required.".into()),
            ..Default::default()
        });
    }

    let site = state.site.get();
    let member = site
        .system_role(SystemRole::Member)
        .ok_or_else(|| AppError::Internal("the Member role is missing".into()))?;
    let status = match mode {
        RegistrationMode::Approval => UserStatus::Pending,
        _ => UserStatus::Active,
    };

    // One transaction, so a failed signup doesn't use up the invite.
    let mut tx = state.db.primary().begin().await?;
    if mode == RegistrationMode::Invite && !invites::redeem(&mut *tx, &form.invite).await? {
        return invalid(RegisterErrors {
            invite: Some("That invite code is invalid, expired or already used.".into()),
            ..Default::default()
        });
    }
    let account = NewAccount {
        name: form.name.trim(),
        password: &form.password,
        email: Some(form.email.as_str()),
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
    tx.commit().await?;
    tracing::info!(user_id = user.id, name = %user.name, ?status, "account registered");

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
}

async fn login_form(page: Page, Query(query): Query<NextQuery>) -> Response {
    if page.current.is_logged_in() {
        return Redirect::to(safe_next(query.next.as_deref())).into_response();
    }
    render_login(&page, "", query.next.as_deref(), None, StatusCode::OK)
}

fn render_login(
    page: &Page,
    name: &str,
    next: Option<&str>,
    error: Option<&str>,
    status: StatusCode,
) -> Response {
    page.render_with_status(
        status,
        "login.html",
        context! { name => name, next => next, error => error },
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
    state
        .rate_limits
        .check_login(info.ip, name)
        .await
        .inspect_err(|_| {
            tracing::warn!(ip = ?info.ip, name, "login rate limited");
        })?;
    let user = match accounts::authenticate(state.db.primary(), name, &form.password).await {
        Ok(user) => user,
        Err(AuthError::Db(error)) => return Err(error.into()),
        Err(error) => {
            let message = match error {
                AuthError::Pending => "Your account is still waiting for approval.",
                AuthError::Deactivated => "This account has been deactivated.",
                _ => "Wrong name or password.",
            };
            tracing::info!(name, reason = %error, "login failed");
            let status = StatusCode::UNPROCESSABLE_ENTITY;
            return Ok(render_login(
                &page,
                name,
                form.next.as_deref(),
                Some(message),
                status,
            ));
        }
    };
    let jar = auth::log_in(&state, jar, &info, &user).await?;
    let jar = flash::set(jar, Flash::LoggedIn);
    Ok((jar, Redirect::to(safe_next(form.next.as_deref()))).into_response())
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
