//! Two-factor login: a code from an authenticator app (or a recovery
//! code) after the password. API keys don't need one.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use minijinja::{Value, context};
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_core::totp::{self, Secret};
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::users::{self, User, UserStatus};
use moekura_db::{accounts, two_factor};
use qrcode::QrCode;
use qrcode::render::svg;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::AppState;
use crate::auth::{self, RequestInfo};
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

/// Holds the token of a login waiting for its code.
const CHALLENGE_COOKIE: &str = "moekura_login";
/// How long someone has to type the code.
const CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
/// Wrong codes before the login has to start over.
const MAX_ATTEMPTS: i32 = 5;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login/code", get(code_form).post(code))
        .route("/settings/two-factor", get(settings))
        .route("/settings/two-factor/setup", post(setup))
        .route("/settings/two-factor/enable", post(enable))
        .route("/settings/two-factor/disable", post(disable))
        .route(
            "/settings/two-factor/recovery-codes",
            post(new_recovery_codes),
        )
        .route("/admin/users/{name}/reset-two-factor", post(admin_reset))
}

fn challenge_cookie(state: &AppState, token: String, max_age: time::Duration) -> Cookie<'static> {
    Cookie::build((CHALLENGE_COOKIE, token))
        .path("/login")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(state.config.server.public_url.scheme() == "https")
        .max_age(max_age)
        .build()
}

/// After the password: if `user` has two-factor login on, answers with
/// the code form instead of logging in.
pub(crate) async fn challenge_if_enabled(
    state: &AppState,
    jar: CookieJar,
    user: &User,
    next: Option<&str>,
) -> Result<Result<CookieJar, Response>, AppError> {
    if !two_factor::is_enabled(state.db.primary(), user.id).await? {
        return Ok(Ok(jar));
    }
    let token = two_factor::challenge(state.db.primary(), user.id, next, CHALLENGE_TTL).await?;
    let max_age = time::Duration::seconds(CHALLENGE_TTL.as_secs() as i64);
    let jar = jar.add(challenge_cookie(state, token, max_age));
    tracing::info!(user_id = user.id, "password accepted, waiting for a code");
    Ok(Err((jar, Redirect::to("/login/code")).into_response()))
}

fn render_code(page: &Page, error: Option<&str>, status: StatusCode) -> Response {
    page.render_with_status(status, "login_code.html", context! { error => error })
}

async fn code_form(page: Page, jar: CookieJar) -> Result<Response, AppError> {
    let waiting = match jar.get(CHALLENGE_COOKIE) {
        Some(cookie) => two_factor::pending(page.state().db.primary(), cookie.value()).await?,
        None => None,
    };
    if waiting.is_none() {
        return Ok(Redirect::to("/login").into_response());
    }
    Ok(render_code(&page, None, StatusCode::OK))
}

#[derive(Debug, Deserialize)]
struct CodeForm {
    #[serde(default)]
    code: String,
}

/// Whether `input` is `user`'s current code or one of their recovery
/// codes, using it up either way.
async fn check_code(state: &AppState, user_id: i64, input: &str) -> Result<bool, AppError> {
    let db = state.db.primary();
    let Some(totp) = two_factor::get(db, user_id).await?.filter(|t| t.enabled) else {
        return Ok(false);
    };
    let now = OffsetDateTime::now_utc().unix_timestamp();
    if let Some(step) = totp.secret.verify(input, now, totp.last_step) {
        return Ok(two_factor::use_step(db, user_id, step).await?);
    }
    let used = two_factor::use_recovery_code(db, user_id, &totp::hash_recovery_code(input)).await?;
    if used {
        tracing::info!(user_id, "logged in with a recovery code");
    }
    Ok(used)
}

async fn code(
    State(state): State<AppState>,
    page: Page,
    jar: CookieJar,
    info: RequestInfo,
    Form(form): Form<CodeForm>,
) -> Result<Response, AppError> {
    let db = state.db.primary();
    let Some(token) = jar.get(CHALLENGE_COOKIE).map(|c| c.value().to_owned()) else {
        return Ok(Redirect::to("/login").into_response());
    };
    let restart = |jar: CookieJar| {
        let jar = jar.remove(challenge_cookie(
            &state,
            String::new(),
            time::Duration::ZERO,
        ));
        (jar, Redirect::to("/login")).into_response()
    };
    let Some(challenge) = two_factor::attempt(db, &token).await? else {
        return Ok(restart(jar));
    };
    let user = users::by_id(db, challenge.user_id).await?;
    let Some(user) = user.filter(|u| u.status == UserStatus::Active) else {
        two_factor::end_challenge(db, &token).await?;
        return Ok(restart(jar));
    };
    if challenge.attempts > MAX_ATTEMPTS {
        two_factor::end_challenge(db, &token).await?;
        tracing::warn!(user_id = user.id, "too many wrong codes");
        return Ok(restart(jar));
    }
    state.rate_limits.check_code(user.id).await?;
    if !check_code(&state, user.id, &form.code).await? {
        tracing::info!(user_id = user.id, "wrong two-factor code");
        return Ok(render_code(
            &page,
            Some("That code isn't right. Check the time on your device, or use a recovery code."),
            StatusCode::UNPROCESSABLE_ENTITY,
        ));
    }
    two_factor::end_challenge(db, &token).await?;
    let jar = jar.remove(challenge_cookie(
        &state,
        String::new(),
        time::Duration::ZERO,
    ));
    let jar = auth::log_in(&state, jar, &info, &user).await?;
    let jar = flash::set(jar, Flash::LoggedIn);
    let next = crate::account::safe_next(challenge.next.as_deref()).to_owned();
    Ok((jar, Redirect::to(&next)).into_response())
}

// ---- settings -------------------------------------------------------------

fn logged_in(page: &Page) -> Result<User, AppError> {
    page.current.user.clone().ok_or(AppError::Unauthorized)
}

/// The two-factor page in one of its states.
#[derive(Default)]
struct View {
    /// While setting up: the QR code and the secret to type in.
    setup: Option<Value>,
    /// Just made: recovery codes to show once.
    recovery_codes: Option<Vec<String>>,
    error: Option<String>,
}

async fn render(
    page: &Page,
    user: &User,
    view: View,
    status: StatusCode,
) -> Result<Response, AppError> {
    let db = page.state().db.primary();
    let enabled = two_factor::is_enabled(db, user.id).await?;
    let unused = if enabled {
        two_factor::unused_recovery_codes(db, user.id).await?
    } else {
        0
    };
    Ok(page.render_with_status(
        status,
        "two_factor.html",
        context! {
            enabled => enabled,
            unused_codes => unused,
            setup => view.setup,
            recovery_codes => view.recovery_codes,
            error => view.error,
        },
    ))
}

async fn settings(page: Page) -> Result<Response, AppError> {
    let user = logged_in(&page)?;
    render(&page, &user, View::default(), StatusCode::OK).await
}

#[derive(Debug, Deserialize)]
struct PasswordForm {
    #[serde(default)]
    password: String,
}

/// Checks the password before a change to two-factor login; `Err` is the
/// page to show instead.
async fn confirm_password(
    page: &Page,
    user: &User,
    password: &str,
) -> Result<Result<(), Response>, AppError> {
    let state = page.state();
    state.rate_limits.check_confirm(user.id).await?;
    if accounts::check_password_of(state.db.primary(), user.id, password).await? {
        return Ok(Ok(()));
    }
    let view = View {
        error: Some("Wrong password.".into()),
        ..View::default()
    };
    Ok(Err(render(
        page,
        user,
        view,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await?))
}

/// The QR code for `uri`, as inline SVG.
fn qr_svg(uri: &str) -> Result<String, AppError> {
    let code = QrCode::new(uri.as_bytes()).map_err(|e| AppError::Internal(e.to_string()))?;
    let image = code
        .render::<svg::Color<'_>>()
        .min_dimensions(200, 200)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    // Inline in the page, so without the XML declaration.
    let start = image.find("<svg").unwrap_or(0);
    Ok(image[start..].to_owned())
}

fn setup_view(
    page: &Page,
    user: &User,
    secret: &Secret,
    error: Option<String>,
) -> Result<View, AppError> {
    let issuer = page.state().site.get().settings.site_name.clone();
    let uri = secret.uri(&issuer, &user.name);
    Ok(View {
        setup: Some(context! {
            qr => Value::from_safe_string(qr_svg(&uri)?),
            secret => secret.to_base32(),
        }),
        error,
        ..View::default()
    })
}

/// Makes a new secret and shows it, to be confirmed with a first code.
async fn setup(page: Page, Form(form): Form<PasswordForm>) -> Result<Response, AppError> {
    let user = logged_in(&page)?;
    if let Err(response) = confirm_password(&page, &user, &form.password).await? {
        return Ok(response);
    }
    let db = page.state().db.primary();
    if two_factor::is_enabled(db, user.id).await? {
        return Ok(Redirect::to("/settings/two-factor").into_response());
    }
    let secret = Secret::generate();
    two_factor::begin(db, user.id, &secret).await?;
    let view = setup_view(&page, &user, &secret, None)?;
    render(&page, &user, view, StatusCode::OK).await
}

/// Turns it on once a code shows the app has the secret, and shows the
/// recovery codes.
async fn enable(page: Page, Form(form): Form<CodeForm>) -> Result<Response, AppError> {
    let user = logged_in(&page)?;
    let state = page.state();
    let db = state.db.primary();
    state.rate_limits.check_confirm(user.id).await?;
    let Some(totp) = two_factor::get(db, user.id).await?.filter(|t| !t.enabled) else {
        return Ok(Redirect::to("/settings/two-factor").into_response());
    };
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let Some(step) = totp.secret.verify(&form.code, now, 0) else {
        let error = Some("That code isn't right. Try the next one your app shows.".to_owned());
        let view = setup_view(&page, &user, &totp.secret, error)?;
        return render(&page, &user, view, StatusCode::UNPROCESSABLE_ENTITY).await;
    };
    let codes = totp::recovery_codes();
    let hashes: Vec<_> = codes.iter().map(|c| totp::hash_recovery_code(c)).collect();
    let mut tx = db.begin().await?;
    two_factor::enable(&mut tx, user.id, step, &hashes).await?;
    tx.commit().await?;
    tracing::info!(user_id = user.id, "two-factor login turned on");
    let view = View {
        recovery_codes: Some(codes),
        ..View::default()
    };
    render(&page, &user, view, StatusCode::OK).await
}

async fn disable(
    page: Page,
    jar: CookieJar,
    Form(form): Form<PasswordForm>,
) -> Result<Response, AppError> {
    let user = logged_in(&page)?;
    if let Err(response) = confirm_password(&page, &user, &form.password).await? {
        return Ok(response);
    }
    let mut conn = page.state().db.primary().acquire().await?;
    if two_factor::disable(&mut conn, user.id).await? {
        tracing::info!(user_id = user.id, "two-factor login turned off");
    }
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/settings/two-factor"),
    )
        .into_response())
}

/// Replaces the recovery codes, when they're used up or may have leaked.
async fn new_recovery_codes(
    page: Page,
    Form(form): Form<PasswordForm>,
) -> Result<Response, AppError> {
    let user = logged_in(&page)?;
    if let Err(response) = confirm_password(&page, &user, &form.password).await? {
        return Ok(response);
    }
    let db = page.state().db.primary();
    if !two_factor::is_enabled(db, user.id).await? {
        return Ok(Redirect::to("/settings/two-factor").into_response());
    }
    let codes = totp::recovery_codes();
    let hashes: Vec<_> = codes.iter().map(|c| totp::hash_recovery_code(c)).collect();
    let mut tx = db.begin().await?;
    two_factor::replace_recovery_codes(&mut tx, user.id, &hashes).await?;
    tx.commit().await?;
    tracing::info!(user_id = user.id, "new recovery codes");
    let view = View {
        recovery_codes: Some(codes),
        ..View::default()
    };
    render(&page, &user, view, StatusCode::OK).await
}

/// For someone who lost their app and their recovery codes: staff who
/// manage users can turn it off, which is logged.
async fn admin_reset(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageUsers)?;
    let state = page.state();
    let db = state.db.primary();
    let user = users::by_name(db, &name).await?.ok_or(AppError::NotFound)?;
    let site = state.site.get();
    let rank = |role_id| site.role(role_id).map_or(0, |r| r.rank);
    let me = page.current.user.as_ref().map(|u| u.id);
    // Like other changes to users: only below one's own rank, and not
    // oneself (that's what the settings page is for).
    if Some(user.id) == me || rank(user.role_id) >= page.current.role.rank {
        return Err(AppError::Forbidden);
    }
    let mut tx = db.begin().await?;
    if two_factor::disable(&mut tx, user.id).await? {
        mod_actions::record(
            &mut *tx,
            NewAction::new(me, ActionKind::UserTwoFactorReset).user(user.id),
        )
        .await?;
        tracing::info!(user_id = user.id, "two-factor login reset by staff");
    }
    tx.commit().await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to("/admin/users")).into_response())
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, TestResponse, member, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(
            test_state(pool).await,
            routes()
                .merge(crate::account::routes())
                .merge(crate::admin::routes())
                .merge(crate::posts::routes()),
        )
    }

    fn now() -> i64 {
        OffsetDateTime::now_utc().unix_timestamp()
    }

    async fn secret(pool: &PgPool, user_id: i64) -> Secret {
        two_factor::get(pool, user_id)
            .await
            .unwrap()
            .unwrap()
            .secret
    }

    /// The `moekura_login` cookie a response sets, as a `Cookie` header.
    fn challenge(response: &TestResponse) -> String {
        response
            .set_cookie
            .iter()
            .find_map(|c| c.strip_prefix("moekura_login="))
            .and_then(|rest| rest.split(';').next())
            .map(|token| format!("moekura_login={token}"))
            .expect("a login challenge cookie")
    }

    async fn log_in(app: &TestApp) -> TestResponse {
        app.post_form(
            "/login",
            None,
            &[],
            "name=alice&password=correct+horse&next=%2Ftags",
        )
        .await
    }

    async fn enter(app: &TestApp, cookie: &str, code: &str) -> TestResponse {
        app.post_form(
            "/login/code",
            None,
            &[("cookie", cookie)],
            &format!(
                "code={}",
                url::form_urlencoded::byte_serialize(code.as_bytes()).collect::<String>()
            ),
        )
        .await
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn setting_up_and_logging_in(pool: PgPool) {
        let app = app(&pool).await;
        let (alice, session) = member(&pool, "alice", "alice@example.com").await;
        let page = app.get("/settings/two-factor", Some(&session)).await;
        assert!(
            page.body.contains("is <strong>off</strong>"),
            "{}",
            page.body
        );

        let wrong = app
            .post_form(
                "/settings/two-factor/setup",
                Some(&session),
                &[],
                "password=nope",
            )
            .await;
        assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
        let setup = app
            .post_form(
                "/settings/two-factor/setup",
                Some(&session),
                &[],
                "password=correct+horse",
            )
            .await;
        assert_eq!(setup.status, StatusCode::OK);
        assert!(setup.body.contains("<svg"), "{}", setup.body);
        assert!(!setup.body.contains("<?xml"));
        let key = secret(&pool, alice.id).await;
        assert!(setup.body.contains(&key.to_base32()));
        // Not on until a code confirms it.
        assert_eq!(log_in(&app).await.location.as_deref(), Some("/tags"));

        let bad = app
            .post_form(
                "/settings/two-factor/enable",
                Some(&session),
                &[],
                "code=000000",
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        let code = key.code_at(now());
        let enabled = app
            .post_form(
                "/settings/two-factor/enable",
                Some(&session),
                &[],
                &format!("code={code}"),
            )
            .await;
        assert_eq!(enabled.status, StatusCode::OK);
        let recovery: Vec<&str> = enabled
            .body
            .split("<code>")
            .skip(1)
            .filter_map(|s| s.split("</code>").next())
            .filter(|s| s.len() == 11)
            .collect();
        assert_eq!(recovery.len(), totp::RECOVERY_CODES, "{}", enabled.body);

        // Now the password alone isn't enough.
        let first = log_in(&app).await;
        assert_eq!(first.location.as_deref(), Some("/login/code"));
        assert!(first.session_cookie().is_none());
        let cookie = challenge(&first);
        assert_eq!(
            app.get_with_cookie("/login/code", &cookie).await.status,
            StatusCode::OK
        );
        assert_eq!(
            app.get("/login/code", None).await.location.as_deref(),
            Some("/login")
        );
        // The code used to turn it on can't be used again.
        let replay = enter(&app, &cookie, &code).await;
        assert_eq!(replay.status, StatusCode::UNPROCESSABLE_ENTITY);
        let next = key.code_at(now() + STEP);
        let done = enter(&app, &cookie, &next).await;
        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.body);
        assert_eq!(done.location.as_deref(), Some("/tags"));
        assert!(done.session_cookie().is_some());
        // The challenge is over.
        assert_eq!(
            enter(&app, &cookie, "123456").await.location.as_deref(),
            Some("/login")
        );

        // A recovery code works once.
        let cookie = challenge(&log_in(&app).await);
        let upper = recovery[0].to_uppercase();
        assert!(
            enter(&app, &cookie, &upper)
                .await
                .session_cookie()
                .is_some()
        );
        let cookie = challenge(&log_in(&app).await);
        assert_eq!(
            enter(&app, &cookie, recovery[0]).await.status,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert!(
            app.get("/settings/two-factor", Some(&session))
                .await
                .body
                .contains("9 unused recovery codes")
        );
    }

    const STEP: i64 = totp::STEP_SECS;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn wrong_codes_end_the_login(pool: PgPool) {
        let app = app(&pool).await;
        let (alice, _) = member(&pool, "alice", "alice@example.com").await;
        let key = Secret::generate();
        two_factor::begin(&pool, alice.id, &key).await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        two_factor::enable(&mut conn, alice.id, 0, &[])
            .await
            .unwrap();

        let cookie = challenge(&log_in(&app).await);
        for _ in 0..MAX_ATTEMPTS {
            let wrong = enter(&app, &cookie, "not a code").await;
            assert_eq!(
                wrong.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{}",
                wrong.body
            );
        }
        // Even the right code is too late now.
        let late = enter(&app, &cookie, &key.code_at(now())).await;
        assert_eq!(late.location.as_deref(), Some("/login"));
        assert!(late.session_cookie().is_none());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn turning_off_and_staff_resets(pool: PgPool) {
        let app = app(&pool).await;
        let (alice, session) = member(&pool, "alice", "alice@example.com").await;
        let turn_on = |id| {
            let pool = pool.clone();
            async move {
                two_factor::begin(&pool, id, &Secret::generate())
                    .await
                    .unwrap();
                let mut conn = pool.acquire().await.unwrap();
                two_factor::enable(&mut conn, id, 0, &[]).await.unwrap();
            }
        };
        turn_on(alice.id).await;
        let off = app
            .post_form(
                "/settings/two-factor/disable",
                Some(&session),
                &[],
                "password=correct+horse",
            )
            .await;
        assert_eq!(off.status, StatusCode::SEE_OTHER);
        assert!(!two_factor::is_enabled(&pool, alice.id).await.unwrap());

        turn_on(alice.id).await;
        let reset = "/admin/users/alice/reset-two-factor";
        let member = session_for(&pool, "bob", SystemRole::Member).await;
        assert_eq!(
            app.post(reset, Some(&member), &[]).await.status,
            StatusCode::FORBIDDEN
        );
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        assert!(
            app.get("/admin/users", Some(&admin))
                .await
                .body
                .contains(reset)
        );
        let done = app.post(reset, Some(&admin), &[]).await;
        assert_eq!(done.status, StatusCode::SEE_OTHER);
        assert!(!two_factor::is_enabled(&pool, alice.id).await.unwrap());
        let logged: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM mod_actions WHERE action = 'user.two_factor_reset' AND user_id = $1",
        )
        .bind(alice.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(logged, 1);
    }
}
