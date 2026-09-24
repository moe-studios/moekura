//! Who is making the request: session cookies or API keys,
//! [`CurrentUser`], logging in and out.

use std::net::IpAddr;
use std::time::Duration;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{AUTHORIZATION, SET_COOKIE, USER_AGENT, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use time::OffsetDateTime;
use uwu_core::permissions::{Permission, Permissions, Role, SystemRole};
use uwu_db::api_keys;
use uwu_db::bans::ActiveBan;
use uwu_db::sessions::{self, Lifetime, NewSession};
use uwu_db::site_cache::SiteSnapshot;
use uwu_db::users::User;

use crate::AppState;
use crate::client_ip::client_ip;
use crate::error::AppError;

pub const SESSION_COOKIE: &str = "uwu_session";

/// Set after a change, for as long as replicas may lag: see
/// [`CurrentUser::recent_write`].
pub const RECENT_WRITE_COOKIE: &str = "uwu_recent";

const DAY: Duration = Duration::from_secs(86_400);

/// The requester. Logged-out visitors get the Anonymous role.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub user: Option<User>,
    /// What the requester may do: their role's, or a visitor's while
    /// banned.
    pub role: Role,
    pub ban: Option<ActiveBan>,
    /// They changed something moments ago, so their reads go to the
    /// primary rather than a replica that may not have it yet.
    pub recent_write: bool,
}

impl CurrentUser {
    fn anonymous(site: &SiteSnapshot) -> Self {
        Self {
            user: None,
            role: anonymous_role(site),
            ban: None,
            recent_write: false,
        }
    }

    pub(crate) fn for_user(user: User, ban: Option<ActiveBan>, site: &SiteSnapshot) -> Self {
        // Banned users, and users whose role was deleted, keep only what
        // visitors can do.
        let role = match ban {
            Some(_) => anonymous_role(site),
            None => site
                .role(user.role_id)
                .cloned()
                .unwrap_or_else(|| anonymous_role(site)),
        };
        Self {
            user: Some(user),
            role,
            ban,
            recent_write: false,
        }
    }

    pub fn is_logged_in(&self) -> bool {
        self.user.is_some()
    }

    pub fn can(&self, permission: Permission) -> bool {
        self.role.can(permission)
    }

    /// `Unauthorized` for visitors who might gain the permission by logging
    /// in, `Forbidden` for users who lack it.
    pub fn require(&self, permission: Permission) -> Result<(), AppError> {
        match (self.can(permission), self.is_logged_in()) {
            (true, _) => Ok(()),
            (false, false) => Err(AppError::Unauthorized),
            (false, true) => Err(AppError::Forbidden),
        }
    }
}

fn anonymous_role(site: &SiteSnapshot) -> Role {
    site.system_role(SystemRole::Anonymous)
        .cloned()
        .unwrap_or(Role {
            id: 0,
            name: "Anonymous".into(),
            permissions: Permissions::NONE,
            rank: 0,
            system: Some(SystemRole::Anonymous),
        })
}

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentUser>()
            .cloned()
            .ok_or_else(|| {
                AppError::Internal("CurrentUser requested outside the session middleware".into())
            })
    }
}

/// The token of an `Authorization: Bearer` header. Other schemes (Basic
/// from a proxy guarding a private site) are left alone.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// Resolves an API key; `None` if it's unknown, revoked or expired.
async fn key_user(state: &AppState, token: &str) -> sqlx::Result<Option<CurrentUser>> {
    let Some(key) = api_keys::lookup(state.db.primary(), token).await? else {
        return Ok(None);
    };
    if key.needs_touch(OffsetDateTime::now_utc())
        && let Err(error) = api_keys::touch(state.db.primary(), key.key_id).await
    {
        tracing::warn!(%error, "could not record API key use");
    }
    Ok(Some(CurrentUser::for_user(
        key.user,
        key.ban,
        &state.site.get(),
    )))
}

/// The answer to an unusable API key. It's refused outright rather than
/// treated as a visitor's request, so a revoked key fails loudly.
async fn refuse_key(path: &str) -> Response {
    let response = (
        StatusCode::UNAUTHORIZED,
        [(WWW_AUTHENTICATE, r#"Bearer error="invalid_token""#)],
        "This API key is invalid, revoked or expired",
    )
        .into_response();
    if crate::api::is_api_path(path) {
        crate::error::json_error(response).await
    } else {
        response
    }
}

/// Middleware: resolves the API key or session cookie into a
/// [`CurrentUser`] request extension, and clears cookies that no longer
/// name a live session.
pub async fn resolve_session(
    State(state): State<AppState>,
    jar: CookieJar,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(token) = bearer_token(request.headers()) {
        let current = match key_user(&state, token).await {
            Ok(Some(current)) => current,
            Ok(None) => return refuse_key(request.uri().path()).await,
            Err(error) => return AppError::from(error).into_response(),
        };
        request.extensions_mut().insert(current);
        return next.run(request).await;
    }

    let site = state.site.get();
    let mut stale_cookie = false;

    let current = match jar.get(SESSION_COOKIE) {
        None => CurrentUser::anonymous(&site),
        Some(cookie) => match sessions::lookup(state.db.primary(), cookie.value()).await {
            Ok(Some(session)) => {
                if session.needs_touch(OffsetDateTime::now_utc()) {
                    // Best effort: failing to extend a session shouldn't fail the request.
                    if let Err(error) =
                        sessions::touch(state.db.primary(), session.session_id, lifetime(&state))
                            .await
                    {
                        tracing::warn!(%error, "could not touch session");
                    }
                }
                CurrentUser::for_user(session.user, session.ban, &site)
            }
            Ok(None) => {
                stale_cookie = true;
                CurrentUser::anonymous(&site)
            }
            Err(error) => return AppError::from(error).into_response(),
        },
    };

    let mut current = current;
    current.recent_write = jar.get(RECENT_WRITE_COOKIE).is_some();
    let changes = !request.method().is_safe();
    request.extensions_mut().insert(current);
    let mut response = next.run(request).await;

    // With replicas, whoever just changed something reads from the primary
    // for a while, so they see their change.
    let status = response.status();
    if changes && state.db.has_replicas() && (status.is_success() || status.is_redirection()) {
        let cookie = Cookie::build((RECENT_WRITE_COOKIE, "1"))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax)
            .secure(secure_cookies(&state))
            .max_age(time::Duration::seconds(
                i64::try_from(state.config.database.replica_max_lag_secs).unwrap_or(i64::MAX),
            ));
        if let Ok(value) = cookie.to_string().parse() {
            response.headers_mut().append(SET_COOKIE, value);
        }
    }

    // Unless the handler just set a fresh session (e.g. logged in).
    if stale_cookie && !sets_session_cookie(&response) {
        let removal = removal_cookie(&state).to_string();
        if let Ok(value) = removal.parse() {
            response.headers_mut().append(SET_COOKIE, value);
        }
    }
    response
}

fn sets_session_cookie(response: &Response) -> bool {
    let prefix = format!("{SESSION_COOKIE}=");
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .any(|v| v.to_str().is_ok_and(|v| v.starts_with(&prefix)))
}

/// Middleware: refuses changes (anything but GET and HEAD, and logging
/// out) from banned networks.
pub async fn block_banned_networks(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if request.method().is_safe() || request.uri().path() == "/logout" {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    if let Some(ip) = client_ip(&parts, &state.config.server.trusted_proxies) {
        match uwu_db::bans::network_ban(state.db.primary(), ip).await {
            Ok(Some(reason)) => {
                return AppError::Blocked(format!(
                    "Your network is banned from making changes: {reason}"
                ))
                .into_response();
            }
            Ok(None) => {}
            Err(error) => return AppError::from(error).into_response(),
        }
    }
    next.run(Request::from_parts(parts, body)).await
}

/// Starts a session for `user` and adds its cookie to `jar`.
pub async fn log_in(
    state: &AppState,
    jar: CookieJar,
    parts: &RequestInfo,
    user: &User,
) -> Result<CookieJar, AppError> {
    let session = NewSession {
        user_id: user.id,
        user_agent: parts.user_agent.as_deref(),
        ip: parts.ip,
    };
    let token = sessions::create(state.db.primary(), session, lifetime(state)).await?;
    Ok(jar.add(session_cookie(state, token)))
}

/// Ends the current session, if any, and removes its cookie.
pub async fn log_out(state: &AppState, jar: CookieJar) -> Result<CookieJar, AppError> {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        sessions::delete(state.db.primary(), cookie.value()).await?;
    }
    Ok(jar.add(removal_cookie(state)))
}

/// Request details recorded with a new session.
#[derive(Debug, Clone, Default)]
pub struct RequestInfo {
    pub user_agent: Option<String>,
    pub ip: Option<IpAddr>,
}

impl FromRequestParts<AppState> for RequestInfo {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user_agent = parts
            .headers
            .get(USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let ip = client_ip(parts, &state.config.server.trusted_proxies);
        Ok(Self { user_agent, ip })
    }
}

fn lifetime(state: &AppState) -> Lifetime {
    let auth = &state.config.auth;
    Lifetime {
        idle: DAY * auth.session_idle_days,
        max: DAY * auth.session_max_days,
    }
}

fn secure_cookies(state: &AppState) -> bool {
    state.config.server.public_url.scheme() == "https"
}

fn session_cookie(state: &AppState, token: String) -> Cookie<'static> {
    // The browser keeps the cookie for the absolute maximum; the server
    // enforces the shorter idle expiry.
    let max_age = time::Duration::days(i64::from(state.config.auth.session_max_days));
    Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure_cookies(state))
        .max_age(max_age)
        .build()
}

fn removal_cookie(state: &AppState) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, ""))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure_cookies(state))
        .max_age(time::Duration::ZERO)
        .build()
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use sqlx::PgPool;
    use uwu_db::users::{self, NewUser, UserStatus};

    use super::*;
    use crate::test_support::{TestApp, test_state};

    async fn whoami(current: CurrentUser) -> String {
        let upload = current.can(Permission::Upload);
        let name = current.user.map_or("anonymous".to_owned(), |u| u.name);
        format!("{name} upload={upload}")
    }

    /// Logs in as the user named in the path, regardless of any cookie.
    async fn login_as(
        State(state): State<AppState>,
        jar: CookieJar,
        info: RequestInfo,
        axum::extract::Path(name): axum::extract::Path<String>,
    ) -> Result<CookieJar, AppError> {
        let user = users::by_name(state.db.primary(), &name)
            .await?
            .ok_or(AppError::NotFound)?;
        log_in(&state, jar, &info, &user).await
    }

    async fn logout(State(state): State<AppState>, jar: CookieJar) -> Result<CookieJar, AppError> {
        log_out(&state, jar).await
    }

    async fn app(pool: &PgPool) -> TestApp {
        let routes = Router::new()
            .route("/whoami", get(whoami))
            .route("/login/{name}", post(login_as))
            .route("/logout", post(logout));
        TestApp::new(test_state(pool).await, routes)
    }

    async fn member(pool: &PgPool, name: &str) -> User {
        let role_id = uwu_db::roles::by_system(pool, SystemRole::Member)
            .await
            .unwrap()
            .id;
        let new = NewUser {
            name,
            email: None,
            password_hash: None,
            role_id,
            status: UserStatus::Active,
        };
        users::insert(pool, new).await.unwrap()
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn visitors_are_anonymous(pool: PgPool) {
        let app = app(&pool).await;
        assert_eq!(
            app.get("/whoami", None).await.body,
            "anonymous upload=false"
        );
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn login_sets_a_cookie_that_identifies_the_user(pool: PgPool) {
        member(&pool, "alice").await;
        let app = app(&pool).await;
        let login = app.post("/login/alice", None, &[]).await;
        let cookie = login.session_cookie().expect("session cookie set");
        assert!(
            login
                .set_cookie
                .iter()
                .any(|c| c.contains("HttpOnly") && c.contains("SameSite=Lax"))
        );
        // http:// public URL, so no Secure flag.
        assert!(!login.set_cookie.iter().any(|c| c.contains("Secure")));

        let me = app.get("/whoami", Some(&cookie)).await;
        assert_eq!(me.body, "alice upload=true");
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn stale_cookies_are_cleared(pool: PgPool) {
        let app = app(&pool).await;
        let response = app.get("/whoami", Some("not-a-session")).await;
        assert_eq!(response.body, "anonymous upload=false");
        assert!(
            response
                .set_cookie
                .iter()
                .any(|c| c.starts_with("uwu_session=;") && c.contains("Max-Age=0"))
        );
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn logging_in_over_a_stale_cookie_keeps_the_new_session(pool: PgPool) {
        member(&pool, "alice").await;
        let app = app(&pool).await;
        let login = app.post("/login/alice", Some("not-a-session"), &[]).await;
        let session_cookies: Vec<_> = login
            .set_cookie
            .iter()
            .filter(|c| c.starts_with("uwu_session="))
            .collect();
        assert_eq!(session_cookies.len(), 1, "{session_cookies:?}");
        let cookie = login.session_cookie().unwrap();
        assert_eq!(
            app.get("/whoami", Some(&cookie)).await.body,
            "alice upload=true"
        );
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn logout_ends_the_session(pool: PgPool) {
        member(&pool, "alice").await;
        let app = app(&pool).await;
        let cookie = app
            .post("/login/alice", None, &[])
            .await
            .session_cookie()
            .unwrap();
        let logout = app.post("/logout", Some(&cookie), &[]).await;
        assert!(logout.set_cookie.iter().any(|c| c.contains("Max-Age=0")));
        assert_eq!(
            app.get("/whoami", Some(&cookie)).await.body,
            "anonymous upload=false"
        );
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn deactivated_users_lose_their_sessions(pool: PgPool) {
        let alice = member(&pool, "alice").await;
        let app = app(&pool).await;
        let cookie = app
            .post("/login/alice", None, &[])
            .await
            .session_cookie()
            .unwrap();
        users::set_status(&pool, alice.id, UserStatus::Deactivated)
            .await
            .unwrap();
        assert_eq!(
            app.get("/whoami", Some(&cookie)).await.body,
            "anonymous upload=false"
        );
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn cross_site_posts_are_rejected(pool: PgPool) {
        member(&pool, "alice").await;
        let app = app(&pool).await;
        let cross_site = app
            .post("/login/alice", None, &[("sec-fetch-site", "cross-site")])
            .await;
        assert_eq!(cross_site.status, StatusCode::FORBIDDEN);
        let foreign_origin = app
            .post("/login/alice", None, &[("origin", "https://evil.example")])
            .await;
        assert_eq!(foreign_origin.status, StatusCode::FORBIDDEN);
        let same_origin = app
            .post("/login/alice", None, &[("sec-fetch-site", "same-origin")])
            .await;
        assert_eq!(same_origin.status, StatusCode::OK);
        let our_origin = app
            .post("/login/alice", None, &[("origin", "http://localhost:8080")])
            .await;
        assert_eq!(our_origin.status, StatusCode::OK);
    }

    #[test]
    fn require_distinguishes_visitors_from_users() {
        let snapshot = SiteSnapshot::new(Default::default(), vec![]);
        let visitor = CurrentUser::anonymous(&snapshot);
        assert!(matches!(
            visitor.require(Permission::Upload),
            Err(AppError::Unauthorized)
        ));
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn changes_pin_reads_to_the_primary_for_a_while(pool: sqlx::PgPool) {
        use crate::test_support::{TestApp, session_for, test_state, test_state_with_replica};
        let alice = session_for(&pool, "alice", uwu_core::permissions::SystemRole::Member).await;
        let form = "per_page=&theme=dark";

        let app = TestApp::new(test_state_with_replica(&pool).await, crate::users::routes());
        let saved = app.post_form("/settings", Some(&alice), &[], form).await;
        let cookie = saved
            .set_cookie
            .iter()
            .find(|c| c.starts_with("uwu_recent="))
            .expect("a change sets the cookie");
        assert!(cookie.contains("Max-Age=10"), "{cookie}");
        let read = app.get("/settings", Some(&alice)).await;
        assert!(!read.set_cookie.iter().any(|c| c.starts_with("uwu_recent=")));

        // Without replicas there's nothing to pin.
        let app = TestApp::new(test_state(&pool).await, crate::users::routes());
        let saved = app.post_form("/settings", Some(&alice), &[], form).await;
        assert!(
            !saved
                .set_cookie
                .iter()
                .any(|c| c.starts_with("uwu_recent="))
        );

        // The cookie sends reads to the primary.
        let state = test_state_with_replica(&pool).await;
        let mut current = CurrentUser::anonymous(&state.site.get());
        assert!(!std::ptr::eq(state.reader(&current), state.db.primary()));
        current.recent_write = true;
        assert!(std::ptr::eq(state.reader(&current), state.db.primary()));
    }
}
