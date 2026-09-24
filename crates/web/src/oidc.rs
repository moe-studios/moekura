//! Logging in through an OpenID Connect provider (`auth.oidc`): the
//! authorization code flow with PKCE.
//!
//! The ID token comes straight from the provider's token endpoint over
//! TLS, so, as OpenID Connect Core 3.1.3.7 allows, its signature isn't
//! checked; its issuer, audience, expiry and nonce are.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use moekura_core::accounts::UserName;
use moekura_core::config::OidcConfig;
use moekura_core::permissions::SystemRole;
use moekura_core::settings::RegistrationMode;
use moekura_core::tokens::NewToken;
use moekura_db::identities::{self, NewLogin, PendingLogin};
use moekura_db::users::{self, InsertError, NewUser, User, UserStatus};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use url::Url;

use crate::AppState;
use crate::auth::{self, RequestInfo};
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

/// How long someone has at the provider before the login is forgotten.
const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);
/// How long the provider's description is reused.
const DISCOVERY_TTL: Duration = Duration::from_secs(60 * 60);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login/oidc", get(start))
        .route("/login/oidc/callback", get(callback))
        .route("/settings/oidc/link", post(link))
        .route("/settings/oidc/unlink/{id}", post(unlink))
}

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("could not reach the provider: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the provider answered {status}: {body}")]
    Status { status: u16, body: String },
    #[error("unexpected answer from the provider: {0}")]
    Invalid(String),
}

/// What the provider says about itself.
#[derive(Debug, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: Url,
    token_endpoint: Url,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Vec<String>,
}

/// The claims of an ID token used here.
#[derive(Debug, Clone, Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: i64,
    nonce: Option<String>,
    azp: Option<String>,
    email: Option<String>,
    /// A boolean, or (from some providers) the string `"true"`.
    #[serde(default)]
    email_verified: serde_json::Value,
    preferred_username: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Claims {
    fn email_verified(&self) -> bool {
        matches!(&self.email_verified, serde_json::Value::Bool(true))
            || self.email_verified.as_str() == Some("true")
    }

    /// Checks the token was meant for us, now, for this login.
    fn check(&self, issuer: &str, client_id: &str, nonce: &str, now: i64) -> Result<(), OidcError> {
        let invalid = |m: &str| Err(OidcError::Invalid(format!("ID token: {m}")));
        if self.iss != issuer {
            return invalid("issued by someone else");
        }
        let audience: Vec<&str> = match &self.aud {
            Audience::One(a) => vec![a],
            Audience::Many(many) => many.iter().map(String::as_str).collect(),
        };
        if !audience.contains(&client_id) {
            return invalid("meant for another client");
        }
        if audience.len() > 1 && self.azp.as_deref() != Some(client_id) {
            return invalid("authorized for another client");
        }
        // A minute's grace for clocks that disagree.
        if self.exp < now - 60 {
            return invalid("expired");
        }
        if self.nonce.as_deref() != Some(nonce) {
            return invalid("for another login");
        }
        Ok(())
    }
}

/// The payload of a JWT, unverified (see the module docs).
fn decode_claims(id_token: &str) -> Result<Claims, OidcError> {
    let payload = id_token
        .split('.')
        .nth(1)
        .ok_or_else(|| OidcError::Invalid("the ID token isn't a JWT".into()))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|e| OidcError::Invalid(format!("ID token: {e}")))?;
    serde_json::from_slice(&bytes).map_err(|e| OidcError::Invalid(format!("ID token: {e}")))
}

/// The provider, and what it said about itself.
pub struct Oidc {
    config: OidcConfig,
    client: reqwest::Client,
    redirect_uri: String,
    discovery: Mutex<Option<(Instant, Arc<Discovery>)>>,
}

impl Oidc {
    pub fn new(config: OidcConfig, public_url: &Url) -> Self {
        moekura_storage::install_crypto_provider();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("moekura/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("the HTTP client configuration is valid");
        let redirect_uri = public_url
            .join("/login/oidc/callback")
            .map_or_else(|_| "/login/oidc/callback".to_owned(), String::from);
        Self {
            config,
            client,
            redirect_uri,
            discovery: Mutex::new(None),
        }
    }

    pub fn button_label(&self) -> &str {
        &self.config.button_label
    }

    async fn discovery(&self) -> Result<Arc<Discovery>, OidcError> {
        let mut cached = self.discovery.lock().await;
        if let Some((fetched, discovery)) = cached.as_ref()
            && fetched.elapsed() < DISCOVERY_TTL
        {
            return Ok(discovery.clone());
        }
        let issuer = self.config.issuer.as_str().trim_end_matches('/');
        let url = format!("{issuer}/.well-known/openid-configuration");
        let response = self.client.get(&url).send().await?;
        let discovery: Discovery = parse(response).await?;
        if discovery.issuer.trim_end_matches('/') != issuer {
            return Err(OidcError::Invalid(format!(
                "the provider calls itself {}, not {issuer}",
                discovery.issuer
            )));
        }
        let discovery = Arc::new(discovery);
        *cached = Some((Instant::now(), discovery.clone()));
        Ok(discovery)
    }

    /// Where to send the browser to log in.
    async fn authorize_url(
        &self,
        state: &str,
        nonce: &str,
        verifier: &str,
    ) -> Result<Url, OidcError> {
        let discovery = self.discovery().await?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = discovery.authorization_endpoint.clone();
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("scope", &self.config.scopes.join(" "))
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url)
    }

    /// Trades the code from the redirect for the ID token's claims.
    async fn exchange(&self, code: &str, login: &PendingLogin) -> Result<Claims, OidcError> {
        let discovery = self.discovery().await?;
        let config = &self.config;
        let methods = &discovery.token_endpoint_auth_methods_supported;
        // Basic is the default when the provider doesn't say.
        let basic = !config.client_secret.is_empty()
            && (methods.is_empty() || methods.iter().any(|m| m == "client_secret_basic"));
        // A block, so the serializer (not Send) is gone before any await.
        let body = {
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            form.append_pair("grant_type", "authorization_code")
                .append_pair("code", code)
                .append_pair("redirect_uri", &self.redirect_uri)
                .append_pair("code_verifier", &login.code_verifier)
                .append_pair("client_id", &config.client_id);
            if !basic && !config.client_secret.is_empty() {
                form.append_pair("client_secret", &config.client_secret);
            }
            form.finish()
        };
        let mut request = self
            .client
            .post(discovery.token_endpoint.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json")
            .body(body);
        if basic {
            // RFC 6749 2.3.1: each part form-encoded first.
            let encode =
                |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
            let credentials = format!(
                "{}:{}",
                encode(&config.client_id),
                encode(&config.client_secret)
            );
            request = request.header(
                "authorization",
                format!("Basic {}", STANDARD.encode(credentials)),
            );
        }
        #[derive(Deserialize)]
        struct Tokens {
            id_token: String,
        }
        let tokens: Tokens = parse(request.send().await?).await?;
        let claims = decode_claims(&tokens.id_token)?;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        claims.check(&discovery.issuer, &config.client_id, &login.nonce, now)?;
        Ok(claims)
    }
}

async fn parse<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, OidcError> {
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        return Err(OidcError::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&body[..body.len().min(500)]).into_owned(),
        });
    }
    serde_json::from_slice(&body).map_err(|e| OidcError::Invalid(e.to_string()))
}

fn provider(state: &AppState) -> Result<&Oidc, AppError> {
    state.oidc.as_deref().ok_or(AppError::NotFound)
}

fn failed(error: OidcError) -> AppError {
    tracing::warn!(%error, "single sign-on failed");
    AppError::BadRequest("Logging in through the provider didn't work. Please try again.".into())
}

/// Sends the browser to the provider; `link_user_id` when linking.
async fn redirect_to_provider(
    state: &AppState,
    next: Option<&str>,
    link_user_id: Option<i64>,
) -> Result<Response, AppError> {
    let oidc = provider(state)?;
    let nonce = NewToken::generate().token;
    let verifier = NewToken::generate().token;
    let login = NewLogin {
        nonce: &nonce,
        code_verifier: &verifier,
        next,
        link_user_id,
    };
    let token = identities::start_login(state.db.primary(), login, LOGIN_TTL).await?;
    let url = oidc
        .authorize_url(&token, &nonce, &verifier)
        .await
        .map_err(failed)?;
    Ok(Redirect::to(url.as_str()).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct StartQuery {
    next: Option<String>,
}

async fn start(
    State(state): State<AppState>,
    page: Page,
    Query(query): Query<StartQuery>,
) -> Result<Response, AppError> {
    let next = crate::account::safe_next(query.next.as_deref());
    if page.current.is_logged_in() {
        return Ok(Redirect::to(next).into_response());
    }
    redirect_to_provider(&state, Some(next), None).await
}

#[derive(Debug, Default, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    error: Option<String>,
    error_description: Option<String>,
}

async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    info: RequestInfo,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let oidc = provider(&state)?;
    let db = state.db.primary();
    let login = identities::finish_login(db, &query.state)
        .await?
        .ok_or_else(|| {
            AppError::BadRequest("This login has expired. Please start again.".into())
        })?;
    if let Some(error) = &query.error {
        tracing::info!(error, description = ?query.error_description, "provider refused the login");
        return Err(AppError::BadRequest(
            "The provider didn't log you in. Please try again.".into(),
        ));
    }
    let claims = oidc.exchange(&query.code, &login).await.map_err(failed)?;

    if let Some(user_id) = login.link_user_id {
        if !identities::link(db, user_id, &claims.iss, &claims.sub).await? {
            return Err(AppError::BadRequest(
                "That account is already linked to a user here.".into(),
            ));
        }
        tracing::info!(user_id, "single sign-on linked");
        let jar = flash::set(jar, Flash::Saved);
        return Ok((jar, Redirect::to("/settings/account")).into_response());
    }

    let user = match identities::user_for(db, &claims.iss, &claims.sub).await? {
        Some(user_id) => users::by_id(db, user_id).await?.ok_or(AppError::NotFound)?,
        None => create_account(&state, &claims).await?,
    };
    let next = crate::account::safe_next(login.next.as_deref()).to_owned();
    match user.status {
        UserStatus::Active => {}
        UserStatus::Pending | UserStatus::Unverified => {
            let jar = flash::set(jar, Flash::AwaitingApproval);
            return Ok((jar, Redirect::to("/")).into_response());
        }
        UserStatus::Deactivated => {
            return Err(AppError::Blocked(
                "This account has been deactivated.".into(),
            ));
        }
    }
    let jar = match crate::two_factor::challenge_if_enabled(&state, jar, &user, Some(&next)).await?
    {
        Ok(jar) => jar,
        Err(code_form) => return Ok(code_form),
    };
    let jar = auth::log_in(&state, jar, &info, &user).await?;
    tracing::info!(user_id = user.id, "logged in through single sign-on");
    Ok((flash::set(jar, Flash::LoggedIn), Redirect::to(&next)).into_response())
}

/// Names to try for a new account, best first.
fn name_candidates(claims: &Claims) -> Vec<String> {
    let clean = |raw: &str| -> String {
        let mapped: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let trimmed = mapped.trim_matches(|c| matches!(c, '_' | '.' | '-'));
        trimmed.chars().take(26).collect()
    };
    let email_name = claims.email.as_deref().and_then(|e| e.split('@').next());
    let bases: Vec<String> = [
        claims.preferred_username.as_deref(),
        email_name,
        claims.name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(clean)
    .filter(|b| b.len() >= 2)
    .collect();
    let mut names: Vec<String> = bases.clone();
    for base in bases
        .iter()
        .take(1)
        .chain(std::iter::once(&"user".to_owned()))
    {
        names.extend((2..=9).map(|n| format!("{base}_{n}")));
        names.push(format!("{base}_{}", &NewToken::generate().token[..6]));
    }
    names.retain(|n| UserName::parse(n).is_ok());
    names.dedup();
    names
}

/// A user for someone logging in for the first time, if the site takes
/// new accounts.
async fn create_account(state: &AppState, claims: &Claims) -> Result<User, AppError> {
    let site = state.site.get();
    let status = match site.settings.registration_mode {
        RegistrationMode::Open => UserStatus::Active,
        RegistrationMode::Approval => UserStatus::Pending,
        RegistrationMode::Invite | RegistrationMode::Closed => {
            return Err(AppError::Blocked(
                "No account here is linked to that login, and this site isn't taking new \
                 accounts. If you have an account, log in with your password and link it \
                 under Settings."
                    .into(),
            ));
        }
    };
    let member = site
        .system_role(SystemRole::Member)
        .ok_or_else(|| AppError::Internal("the Member role is missing".into()))?;
    let mut tx = state.db.primary().begin().await?;
    let mut created = None;
    for name in name_candidates(claims) {
        let new = NewUser {
            name: &name,
            email: None,
            password_hash: None,
            role_id: member.id,
            status,
        };
        // A savepoint: a taken name aborts the statement, which would
        // otherwise abort the whole transaction.
        let mut attempt = sqlx::Connection::begin(&mut *tx).await?;
        match users::insert(&mut *attempt, new).await {
            Ok(user) => {
                attempt.commit().await?;
                created = Some(user);
                break;
            }
            Err(InsertError::NameTaken) => attempt.rollback().await?,
            Err(e) => return Err(AppError::Internal(e.to_string())),
        }
    }
    let mut user =
        created.ok_or_else(|| AppError::Internal("no free name for a new account".into()))?;
    // The provider's word that the address is theirs is good enough.
    if let Some(email) = claims.email.as_deref().filter(|_| claims.email_verified())
        && users::by_email(&mut *tx, email).await?.is_none()
    {
        users::set_email(&mut *tx, user.id, Some(email), true)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        user.email = Some(email.to_owned());
    }
    identities::link(&mut *tx, user.id, &claims.iss, &claims.sub).await?;
    tx.commit().await?;
    tracing::info!(user_id = user.id, name = %user.name, ?status, "account created through single sign-on");
    Ok(user)
}

#[derive(Debug, Deserialize)]
struct PasswordForm {
    #[serde(default)]
    password: String,
}

/// Links a provider account to the logged-in user, after checking their
/// password: a link is a way in.
async fn link(page: Page, Form(form): Form<PasswordForm>) -> Result<Response, AppError> {
    let user = page.current.user.clone().ok_or(AppError::Unauthorized)?;
    let state = page.state();
    provider(state)?;
    state.rate_limits.check_confirm(user.id).await?;
    let db = state.db.primary();
    if users::has_password(db, user.id).await?
        && !moekura_db::accounts::check_password_of(db, user.id, &form.password).await?
    {
        return Err(AppError::Unprocessable("Wrong password.".into()));
    }
    redirect_to_provider(state, None, Some(user.id)).await
}

/// Unlinks a provider account. Accounts without a password keep their
/// last one, or they couldn't log in any more.
async fn unlink(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    let user = page.current.user.clone().ok_or(AppError::Unauthorized)?;
    let db = page.state().db.primary();
    let linked = identities::for_user(db, user.id).await?;
    if linked.len() <= 1 && !users::has_password(db, user.id).await? {
        return Err(AppError::Unprocessable(
            "This is how you log in: set a password first (with “Forgot your password?”).".into(),
        ));
    }
    if !identities::unlink(db, user.id, id).await? {
        return Err(AppError::NotFound);
    }
    tracing::info!(user_id = user.id, "single sign-on unlinked");
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/settings/account"),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use axum::Json;
    use axum::http::{HeaderMap, StatusCode};
    use moekura_core::permissions::SystemRole;
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, member, session_for, test_config, test_state_with};

    /// A provider that logs in whoever the test says.
    #[derive(Default)]
    struct Provider {
        base: String,
        challenge: StdMutex<Option<String>>,
        claims: StdMutex<serde_json::Value>,
    }

    async fn provider() -> Arc<Provider> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let fake = Arc::new(Provider {
            base: base.clone(),
            ..Provider::default()
        });
        let discovery = json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "token_endpoint_auth_methods_supported": ["client_secret_basic"],
        });
        let shared = fake.clone();
        let router = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || async move { Json(discovery) }),
            )
            .route(
                "/token",
                post(move |headers: HeaderMap, body: String| {
                    let fake = shared.clone();
                    async move {
                        let form: std::collections::HashMap<String, String> =
                            url::form_urlencoded::parse(body.as_bytes())
                                .into_owned()
                                .collect();
                        let basic = format!("Basic {}", STANDARD.encode("moekura:hush"));
                        let verifier = form.get("code_verifier").cloned().unwrap_or_default();
                        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
                        let expected = fake.challenge.lock().unwrap().clone();
                        let ok = headers.get("authorization").and_then(|v| v.to_str().ok())
                            == Some(basic.as_str())
                            && form.get("code").map(String::as_str) == Some("good")
                            && Some(challenge) == expected;
                        if !ok {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({ "error": "invalid_grant" })),
                            );
                        }
                        let claims = fake.claims.lock().unwrap().clone();
                        let token = format!(
                            "{}.{}.sig",
                            URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
                            URL_SAFE_NO_PAD.encode(claims.to_string()),
                        );
                        (
                            StatusCode::OK,
                            Json(json!({ "id_token": token, "token_type": "Bearer" })),
                        )
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        fake
    }

    async fn app(pool: &PgPool, fake: &Provider) -> TestApp {
        let mut config = test_config();
        config.auth.oidc = Some(OidcConfig {
            issuer: Url::parse(&fake.base).unwrap(),
            client_id: "moekura".into(),
            client_secret: "hush".into(),
            button_label: "Log in with Fake".into(),
            scopes: vec!["openid".into(), "email".into()],
        });
        TestApp::new(
            test_state_with(pool, config).await,
            routes()
                .merge(crate::account::routes())
                .merge(crate::email::routes())
                .merge(crate::posts::routes()),
        )
    }

    /// Follows a redirect to the provider: its `state`, after telling the
    /// provider to log in as `claims` (with the right nonce).
    fn at_provider(
        fake: &Provider,
        response: &crate::test_support::TestResponse,
        mut claims: serde_json::Value,
    ) -> String {
        let location = response.location.as_deref().expect("a redirect");
        let url = Url::parse(location).unwrap();
        assert_eq!(url.path(), "/authorize");
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        assert_eq!(param("client_id").as_deref(), Some("moekura"));
        assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(
            param("redirect_uri").as_deref(),
            Some("http://localhost:8080/login/oidc/callback")
        );
        *fake.challenge.lock().unwrap() = param("code_challenge");
        if claims.get("nonce").is_none() {
            claims["nonce"] = json!(param("nonce").unwrap());
        }
        *fake.claims.lock().unwrap() = claims;
        param("state").unwrap()
    }

    fn claims(fake: &Provider, sub: &str) -> serde_json::Value {
        json!({
            "iss": fake.base,
            "sub": sub,
            "aud": "moekura",
            "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
            "preferred_username": "Alice Smith",
            "email": "alice@example.com",
            "email_verified": true,
        })
    }

    async fn back(
        app: &TestApp,
        state: &str,
        session: Option<&str>,
    ) -> crate::test_support::TestResponse {
        app.get(
            &format!("/login/oidc/callback?code=good&state={state}"),
            session,
        )
        .await
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn first_login_makes_an_account(pool: PgPool) {
        let fake = provider().await;
        let app = app(&pool, &fake).await;
        assert!(
            app.get("/login", None)
                .await
                .body
                .contains("Log in with Fake")
        );

        let start = app.get("/login/oidc?next=%2Ftags", None).await;
        let state = at_provider(&fake, &start, claims(&fake, "u1"));
        let done = back(&app, &state, None).await;
        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.body);
        assert_eq!(done.location.as_deref(), Some("/tags"));
        assert!(done.session_cookie().is_some());
        let user = users::by_name(&pool, "Alice_Smith").await.unwrap().unwrap();
        assert_eq!(user.email.as_deref(), Some("alice@example.com"));
        assert!(user.email_verified_at.is_some());
        assert!(!users::has_password(&pool, user.id).await.unwrap());
        // The state works once.
        assert_eq!(
            back(&app, &state, None).await.status,
            StatusCode::BAD_REQUEST
        );

        // Next time, the same account.
        let again = app.get("/login/oidc", None).await;
        let state = at_provider(&fake, &again, claims(&fake, "u1"));
        assert!(back(&app, &state, None).await.session_cookie().is_some());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);

        // Another person with the same name gets the next one that's free,
        // and not the address someone already has.
        for (sub, name) in [("u2", "alice"), ("u3", "Alice_Smith_2")] {
            let other = app.get("/login/oidc", None).await;
            let state = at_provider(&fake, &other, claims(&fake, sub));
            let response = back(&app, &state, None).await;
            assert_eq!(
                response.status,
                StatusCode::SEE_OTHER,
                "{sub}: {}",
                response.body
            );
            let user = users::by_name(&pool, name).await.unwrap().unwrap();
            assert_eq!(user.email, None, "the address is taken");
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn refuses_tokens_that_dont_fit(pool: PgPool) {
        let fake = provider().await;
        let app = app(&pool, &fake).await;
        let cases = [
            ("aud", json!("someone-else")),
            ("iss", json!("https://evil.example.com")),
            ("nonce", json!("replayed")),
            ("exp", json!(1_000_000)),
        ];
        for (claim, value) in cases {
            let start = app.get("/login/oidc", None).await;
            let mut bad = claims(&fake, "u1");
            bad[claim] = value;
            let state = at_provider(&fake, &start, bad);
            let refused = back(&app, &state, None).await;
            assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{claim}");
            assert!(refused.session_cookie().is_none());
        }
        // A code the provider doesn't accept.
        let start = app.get("/login/oidc", None).await;
        let state = at_provider(&fake, &start, claims(&fake, "u1"));
        let wrong = app
            .get(
                &format!("/login/oidc/callback?code=bad&state={state}"),
                None,
            )
            .await;
        assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn follows_the_registration_mode(pool: PgPool) {
        let fake = provider().await;
        moekura_db::settings::set(&pool, "registration_mode", json!("closed"))
            .await
            .unwrap();
        let closed = app(&pool, &fake).await;
        let start = closed.get("/login/oidc", None).await;
        let state = at_provider(&fake, &start, claims(&fake, "u1"));
        assert_eq!(
            back(&closed, &state, None).await.status,
            StatusCode::FORBIDDEN
        );

        moekura_db::settings::set(&pool, "registration_mode", json!("approval"))
            .await
            .unwrap();
        let approval = app(&pool, &fake).await;
        let start = approval.get("/login/oidc", None).await;
        let state = at_provider(&fake, &start, claims(&fake, "u1"));
        let waiting = back(&approval, &state, None).await;
        assert!(waiting.session_cookie().is_none());
        let user = users::by_name(&pool, "Alice_Smith").await.unwrap().unwrap();
        assert_eq!(user.status, UserStatus::Pending);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn linking_an_existing_account(pool: PgPool) {
        let fake = provider().await;
        let app = app(&pool, &fake).await;
        let (alice, session) = member(&pool, "alice", "alice@example.com").await;
        assert!(
            app.get("/settings/account", Some(&session))
                .await
                .body
                .contains("/settings/oidc/link")
        );

        let wrong = app
            .post_form("/settings/oidc/link", Some(&session), &[], "password=nope")
            .await;
        assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
        let start = app
            .post_form(
                "/settings/oidc/link",
                Some(&session),
                &[],
                "password=correct+horse",
            )
            .await;
        let state = at_provider(&fake, &start, claims(&fake, "u1"));
        let linked = back(&app, &state, Some(&session)).await;
        assert_eq!(
            linked.location.as_deref(),
            Some("/settings/account"),
            "{}",
            linked.body
        );

        // Logging in through the provider is now logging in as alice.
        let start = app.get("/login/oidc", None).await;
        let state = at_provider(&fake, &start, claims(&fake, "u1"));
        assert!(back(&app, &state, None).await.session_cookie().is_some());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);

        // Someone else can't link the same provider account.
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let start = app
            .post_form("/settings/oidc/link", Some(&bob), &[], "")
            .await;
        let state = at_provider(&fake, &start, claims(&fake, "u1"));
        assert_eq!(
            back(&app, &state, Some(&bob)).await.status,
            StatusCode::BAD_REQUEST
        );

        // Alice has a password, so she can unlink it; bob (no password)
        // can't unlink his only way in.
        let id = identities::for_user(&pool, alice.id).await.unwrap()[0].id;
        let unlinked = app
            .post(&format!("/settings/oidc/unlink/{id}"), Some(&session), &[])
            .await;
        assert_eq!(unlinked.status, StatusCode::SEE_OTHER);
        let bob_id = users::by_name(&pool, "bob").await.unwrap().unwrap().id;
        identities::link(&pool, bob_id, &fake.base, "b1")
            .await
            .unwrap();
        let id = identities::for_user(&pool, bob_id).await.unwrap()[0].id;
        let kept = app
            .post(&format!("/settings/oidc/unlink/{id}"), Some(&bob), &[])
            .await;
        assert_eq!(kept.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn names_for_new_accounts() {
        let claims: Claims = serde_json::from_value(json!({
            "iss": "x", "sub": "1", "aud": "a", "exp": 0,
            "preferred_username": "admin",
            "email": "Jo Bloggs+x@example.com",
            "name": "Jo",
        }))
        .unwrap();
        let names = name_candidates(&claims);
        // `admin` is reserved, and `Jo` too short.
        assert_eq!(names[0], "Jo_Bloggs_x");
        assert!(names.contains(&"admin_2".to_owned()));
        assert!(names.iter().all(|n| UserName::parse(n).is_ok()));
    }
}
