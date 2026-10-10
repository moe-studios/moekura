//! Passkeys (WebAuthn): adding them on the account page, and logging in
//! with one, either without the password (the browser offers the site's
//! passkeys, and the one picked says whose account it is) or instead of a
//! two-factor code after it. A passkey checks that the person is there and
//! unlocked the device (user verification), so it skips the second step.
//!
//! The relying party is the host in `server.public_url`: passkeys made for
//! one domain don't work on another. The browser's side is
//! `frontend/src/passkeys.ts`, which these routes answer with JSON.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum::{Form, Json, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_db::accounts::AuthError;
use moekura_db::passkeys::{self as stored, MAX_PER_USER, NAME_MAX_LEN, NewPasskey, Purpose};
use moekura_db::users::{self, User, UserStatus};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;
use webauthn_rs::prelude::{
    CreationChallengeResponse, CredentialID, DiscoverableAuthentication, DiscoverableKey, Passkey,
    PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    Uuid, Webauthn, WebauthnBuilder, WebauthnError,
};

use crate::AppState;
use crate::auth::{self, RequestInfo};
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

#[cfg(test)]
mod authenticator;

/// How long the browser has to answer.
const CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);

/// Where the account page lists them.
const ACCOUNT_PAGE: &str = "/settings/account#passkeys";

/// The site as a WebAuthn relying party.
pub(crate) struct Passkeys {
    webauthn: Webauthn,
}

impl Passkeys {
    /// For a site at `public_url`. `None`, with a warning, when its host
    /// can't be a relying party ID: browsers only take domain names (and
    /// `localhost`), not addresses.
    pub(crate) fn new(public_url: &Url) -> Option<Self> {
        let Some(rp_id) = public_url.domain() else {
            tracing::warn!(
                %public_url,
                "passkeys are off: server.public_url needs a domain name, not an address"
            );
            return None;
        };
        let built = Url::parse(&public_url.origin().ascii_serialization())
            .map_err(|_| WebauthnError::Configuration)
            .and_then(|origin| WebauthnBuilder::new(rp_id, &origin)?.build());
        match built {
            Ok(webauthn) => Some(Self { webauthn }),
            Err(error) => {
                tracing::warn!(%error, %public_url, "passkeys are off");
                None
            }
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings/passkeys/options", post(register_options))
        .route("/settings/passkeys", post(register))
        .route("/settings/passkeys/rename", post(rename))
        .route("/settings/passkeys/remove", post(remove))
        .route("/login/passkey/options", post(login_options))
        .route("/login/passkey", post(login))
        .route("/login/code/passkey/options", post(second_factor_options))
        .route("/login/code/passkey", post(second_factor))
}

fn enabled(state: &AppState) -> Result<&Passkeys, AppError> {
    state.passkeys.as_deref().ok_or(AppError::NotFound)
}

/// A refusal, which the page's script shows as it is.
fn refuse(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Sends the browser on to `to`.
fn go(jar: CookieJar, to: &str) -> Response {
    (jar, Json(json!({ "redirect": to }))).into_response()
}

/// What the browser needs to ask the authenticator, and the token to send
/// its answer back with.
fn ask(token: String, options: impl serde::Serialize) -> Response {
    Json(json!({ "token": token, "options": options })).into_response()
}

fn internal(error: impl std::fmt::Display) -> AppError {
    AppError::Internal(error.to_string())
}

fn to_json(value: &impl serde::Serialize) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(value).map_err(internal)
}

fn from_json<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(value).map_err(internal)
}

/// `name` trimmed, if it's a usable name for a passkey.
fn valid_name(name: &str) -> Option<&str> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= NAME_MAX_LEN).then_some(name)
}

/// Asks for a discoverable credential (one the authenticator offers at
/// login without being told whose account it's for) where the
/// authenticator can make one: logging in without a name needs that.
/// webauthn-rs asks for none for plain passkeys. A security key that can't
/// keep one still works after the password.
fn prefer_discoverable(options: &CreationChallengeResponse) -> Result<serde_json::Value, AppError> {
    let mut options = to_json(options)?;
    if let Some(selection) = options
        .pointer_mut("/publicKey/authenticatorSelection")
        .and_then(serde_json::Value::as_object_mut)
    {
        selection.insert("residentKey".into(), "preferred".into());
        selection.insert("requireResidentKey".into(), false.into());
    }
    Ok(options)
}

// ---- the account page -----------------------------------------------------

/// The passkeys part of the account page.
pub(crate) async fn account_context(page: &Page, user: &User) -> Result<Value, AppError> {
    let db = page.state().db.primary();
    let list = stored::for_user(db, user.id).await?;
    Ok(context! {
        enabled => page.state().passkeys.is_some(),
        full => list.len() as i64 >= MAX_PER_USER,
        max => MAX_PER_USER,
        name_max => NAME_MAX_LEN,
        has_password => users::has_password(db, user.id).await?,
        fresh_login_minutes => auth::FRESH_LOGIN.as_secs() / 60,
        list => list.iter().map(|p| context! {
            id => p.id,
            name => p.name,
            created => crate::dates::day(p.created_at),
            last_used => p.last_used_at.map(crate::dates::day),
        }).collect::<Vec<_>>(),
    })
}

/// The account page again, with `error` about passkeys.
async fn account_with_error(page: &Page, user: &User, error: String) -> Result<Response, AppError> {
    let methods = crate::email::login_methods(page, user).await?;
    let errors = crate::email::AccountErrors {
        passkeys: Some(error),
        ..Default::default()
    };
    Ok(crate::email::render_account(
        page,
        user,
        methods,
        &errors,
        StatusCode::UNPROCESSABLE_ENTITY,
    ))
}

/// A passkey on the way: kept on the server until the browser answers.
#[derive(serde::Serialize, Deserialize)]
struct Registration {
    name: String,
    /// The user handle it's made with.
    handle: Uuid,
    state: PasskeyRegistration,
}

#[derive(Debug, Deserialize)]
struct NewPasskeyForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    password: String,
}

/// Starts adding a passkey, once the password confirms it's the owner.
async fn register_options(
    page: Page,
    Json(form): Json<NewPasskeyForm>,
) -> Result<Response, AppError> {
    let state = page.state();
    let passkeys = enabled(state)?;
    // Not with an API key: they can't change how the account logs in.
    let user = page.current.require_session()?;
    let Some(name) = valid_name(&form.name) else {
        let max = NAME_MAX_LEN.to_string();
        let message = page.say("passkeys-name-invalid", &[("max", &max)]);
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    };
    if let Err(message) = auth::confirm_password(state, &page.current, &form.password).await? {
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    }
    let db = state.db.primary();
    let existing = stored::for_user(db, user.id).await?;
    if existing.len() as i64 >= MAX_PER_USER {
        let max = MAX_PER_USER.to_string();
        let message = page.say("passkeys-full", &[("max", &max)]);
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    }
    // One handle per account, so the authenticator keeps one passkey for
    // it rather than one for each.
    let handle = existing
        .first()
        .and_then(|p| Uuid::from_slice(&p.user_handle).ok())
        .unwrap_or_else(Uuid::new_v4);
    let exclude: Vec<CredentialID> = existing
        .iter()
        .map(|p| CredentialID::from(p.credential_id.clone()))
        .collect();
    let (options, registration) = passkeys
        .webauthn
        .start_passkey_registration(handle, &user.name, &user.name, Some(exclude))
        .map_err(internal)?;
    let pending = Registration {
        name: name.to_owned(),
        handle,
        state: registration,
    };
    let token = stored::challenge(
        db,
        Some(user.id),
        Purpose::Register,
        &to_json(&pending)?,
        CHALLENGE_TTL,
    )
    .await?;
    Ok(ask(token, prefer_discoverable(&options)?))
}

#[derive(Debug, Deserialize)]
struct RegistrationAnswer {
    #[serde(default)]
    token: String,
    credential: RegisterPublicKeyCredential,
}

/// Adds the passkey the authenticator made.
async fn register(
    page: Page,
    jar: CookieJar,
    Json(answer): Json<RegistrationAnswer>,
) -> Result<Response, AppError> {
    let state = page.state();
    let passkeys = enabled(state)?;
    let user = page.current.require_session()?;
    let db = state.db.primary();
    let Some(pending) =
        stored::take_challenge(db, &answer.token, Purpose::Register, Some(user.id)).await?
    else {
        let message = page.say("passkeys-expired", &[]);
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    };
    let pending: Registration = from_json(pending)?;
    let passkey = match passkeys
        .webauthn
        .finish_passkey_registration(&answer.credential, &pending.state)
    {
        Ok(passkey) => passkey,
        Err(error) => {
            tracing::info!(user_id = user.id, %error, "passkey not added");
            let message = page.say("passkeys-not-added", &[]);
            return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
        }
    };
    let added = stored::add(
        db,
        NewPasskey {
            user_id: user.id,
            credential_id: passkey.cred_id().as_ref(),
            user_handle: pending.handle.as_bytes(),
            name: &pending.name,
            credential: &to_json(&passkey)?,
        },
    )
    .await;
    let id = match added {
        Ok(id) => id,
        Err(stored::AddError::Taken) => {
            let message = page.say("passkeys-taken", &[]);
            return Ok(refuse(StatusCode::CONFLICT, message));
        }
        Err(stored::AddError::Db(error)) => return Err(error.into()),
    };
    tracing::info!(user_id = user.id, passkey_id = id, "passkey added");
    Ok(go(flash::set(jar, Flash::Saved), ACCOUNT_PAGE))
}

#[derive(Debug, Deserialize)]
struct RenameForm {
    id: i64,
    #[serde(default)]
    name: String,
}

async fn rename(
    page: Page,
    jar: CookieJar,
    Form(form): Form<RenameForm>,
) -> Result<Response, AppError> {
    let user = page.current.require_session()?.clone();
    let Some(name) = valid_name(&form.name) else {
        let max = NAME_MAX_LEN.to_string();
        let message = page.say("passkeys-name-invalid", &[("max", &max)]);
        return account_with_error(&page, &user, message).await;
    };
    if !stored::rename(page.state().db.primary(), user.id, form.id, name).await? {
        return Err(AppError::NotFound);
    }
    Ok((flash::set(jar, Flash::Saved), Redirect::to(ACCOUNT_PAGE)).into_response())
}

#[derive(Debug, Deserialize)]
struct RemoveForm {
    id: i64,
    #[serde(default)]
    password: String,
}

/// Removes a passkey, once the password confirms it's the owner.
async fn remove(
    page: Page,
    jar: CookieJar,
    Form(form): Form<RemoveForm>,
) -> Result<Response, AppError> {
    let user = page.current.require_session()?.clone();
    let state = page.state();
    if let Err(message) = auth::confirm_password(state, &page.current, &form.password).await? {
        return account_with_error(&page, &user, message).await;
    }
    if !stored::remove(state.db.primary(), user.id, form.id).await? {
        return Err(AppError::NotFound);
    }
    tracing::info!(user_id = user.id, passkey_id = form.id, "passkey removed");
    Ok((flash::set(jar, Flash::Saved), Redirect::to(ACCOUNT_PAGE)).into_response())
}

// ---- logging in -----------------------------------------------------------

/// Starts a login with whichever of the site's passkeys the browser
/// offers. The script asks for one when the login page opens (to offer
/// them as the name field's suggestions) and again for the button.
async fn login_options(
    State(state): State<AppState>,
    info: RequestInfo,
) -> Result<Response, AppError> {
    let passkeys = enabled(&state)?;
    state.rate_limits.check_passkey_start(info.ip).await?;
    let (mut options, pending) = passkeys
        .webauthn
        .start_discoverable_authentication()
        .map_err(internal)?;
    // The script says how to ask: in the name field's suggestions, or
    // in a dialog of the browser's own.
    options.mediation = None;
    let token = stored::challenge(
        state.db.primary(),
        None,
        Purpose::Login,
        &to_json(&pending)?,
        CHALLENGE_TTL,
    )
    .await?;
    Ok(ask(token, options))
}

#[derive(Debug, Deserialize)]
struct LoginAnswer {
    #[serde(default)]
    token: String,
    credential: PublicKeyCredential,
    next: Option<String>,
}

/// Logs in with the passkey the browser picked.
async fn login(
    State(state): State<AppState>,
    page: Page,
    jar: CookieJar,
    info: RequestInfo,
    Json(answer): Json<LoginAnswer>,
) -> Result<Response, AppError> {
    let passkeys = enabled(&state)?;
    let db = state.db.primary();
    let Some(pending) = stored::take_challenge(db, &answer.token, Purpose::Login, None).await?
    else {
        let message = page.say("passkeys-expired", &[]);
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    };
    let pending: DiscoverableAuthentication = from_json(pending)?;
    // The authenticator says which passkey, and so whose account; the
    // signature checked below proves it.
    let found = match passkeys
        .webauthn
        .identify_discoverable_authentication(&answer.credential)
    {
        Ok((handle, credential_id)) => stored::by_credential_id(db, credential_id)
            .await?
            .filter(|p| p.user_handle == handle.as_bytes()),
        Err(_) => None,
    };
    let user = match &found {
        Some(passkey) => users::by_id(db, passkey.user_id).await?,
        None => None,
    };
    // Counted like a password, before it's checked: per network, and per
    // account from it. Not against the ceiling for an account from all
    // networks: that's for passwords guessed from many, and a passkey
    // can't be guessed, so its owner still gets in meanwhile.
    let name = user.as_ref().map_or("", |u| u.name.as_str());
    state.rate_limits.check_login(info.ip, name).await?;
    let (Some(found), Some(user)) = (found, user) else {
        tracing::info!(reason = "unknown passkey", "passkey login failed");
        let message = page.say("passkeys-unknown", &[]);
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    };
    let mut passkey: Passkey = from_json(found.credential)?;
    let checked = passkeys.webauthn.finish_discoverable_authentication(
        &answer.credential,
        pending,
        &[DiscoverableKey::from(&passkey)],
    );
    let result = match checked {
        Ok(result) => result,
        Err(error) => {
            refused(user.id, Some(found.id), &error);
            let message = page.say("passkeys-failed", &[]);
            return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
        }
    };
    let status = match user.status {
        UserStatus::Active => None,
        UserStatus::Pending => Some(AuthError::Pending),
        UserStatus::Unverified => Some(AuthError::Unverified),
        UserStatus::Deactivated => Some(AuthError::Deactivated),
    };
    if let Some(error) = status {
        tracing::info!(user_id = user.id, reason = %error, "passkey login failed");
        let message = crate::account::refusal(&error).to_owned();
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    }
    passkey.update_credential(&result);
    stored::used(db, found.id, &to_json(&passkey)?).await?;
    let jar = auth::log_in(&state, jar, &info, &user).await?;
    tracing::info!(
        user_id = user.id,
        passkey_id = found.id,
        "logged in with a passkey"
    );
    let next = crate::account::safe_next(answer.next.as_deref());
    Ok(go(flash::set(jar, Flash::LoggedIn), next))
}

/// Logs why a passkey's answer was refused. A sign count that went back
/// means the passkey may have been copied.
fn refused(user_id: i64, passkey_id: Option<i64>, error: &WebauthnError) {
    if matches!(error, WebauthnError::CredentialPossibleCompromise) {
        tracing::warn!(
            user_id,
            passkey_id,
            "passkey refused: its sign count went back, so it may have been copied"
        );
    } else {
        tracing::info!(user_id, passkey_id, %error, "passkey refused");
    }
}

// ---- after the password ---------------------------------------------------

/// The login waiting for its second step is gone (expired, or too many
/// tries): back to the password.
fn restart(state: &AppState, jar: CookieJar) -> Response {
    let jar = jar.remove(crate::two_factor::challenge_cookie(
        state,
        String::new(),
        time::Duration::ZERO,
    ));
    (StatusCode::CONFLICT, go(jar, "/login")).into_response()
}

/// Starts checking one of the user's passkeys instead of a code, for the
/// login waiting for its second step.
async fn second_factor_options(
    State(state): State<AppState>,
    page: Page,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let passkeys = enabled(&state)?;
    let db = state.db.primary();
    let waiting = match jar.get(crate::two_factor::CHALLENGE_COOKIE) {
        Some(cookie) => moekura_db::two_factor::pending(db, cookie.value()).await?,
        None => None,
    };
    let Some(waiting) = waiting else {
        return Ok(restart(&state, jar));
    };
    let credentials = stored::for_user(db, waiting.user_id)
        .await?
        .into_iter()
        .map(|p| from_json::<Passkey>(p.credential))
        .collect::<Result<Vec<_>, _>>()?;
    if credentials.is_empty() {
        let message = page.say("passkeys-none", &[]);
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    }
    let (options, pending) = passkeys
        .webauthn
        .start_passkey_authentication(&credentials)
        .map_err(internal)?;
    let token = stored::challenge(
        db,
        Some(waiting.user_id),
        Purpose::SecondFactor,
        &to_json(&pending)?,
        CHALLENGE_TTL,
    )
    .await?;
    Ok(ask(token, options))
}

#[derive(Debug, Deserialize)]
struct SecondFactorAnswer {
    #[serde(default)]
    token: String,
    credential: PublicKeyCredential,
}

/// Finishes the login with a passkey instead of a code. Counted like a
/// code: against the login's tries, and the user's rate of codes.
async fn second_factor(
    State(state): State<AppState>,
    page: Page,
    jar: CookieJar,
    info: RequestInfo,
    Json(answer): Json<SecondFactorAnswer>,
) -> Result<Response, AppError> {
    let passkeys = enabled(&state)?;
    let db = state.db.primary();
    let Some(login) = jar
        .get(crate::two_factor::CHALLENGE_COOKIE)
        .map(|c| c.value().to_owned())
    else {
        return Ok(restart(&state, jar));
    };
    let Some(challenge) = moekura_db::two_factor::attempt(db, &login).await? else {
        return Ok(restart(&state, jar));
    };
    let user = users::by_id(db, challenge.user_id).await?;
    let Some(user) = user.filter(|u| u.status == UserStatus::Active) else {
        moekura_db::two_factor::end_challenge(db, &login).await?;
        return Ok(restart(&state, jar));
    };
    if challenge.attempts > crate::two_factor::MAX_ATTEMPTS {
        moekura_db::two_factor::end_challenge(db, &login).await?;
        tracing::warn!(user_id = user.id, "too many tries at the second step");
        return Ok(restart(&state, jar));
    }
    state.rate_limits.check_code(user.id).await?;
    let pending =
        stored::take_challenge(db, &answer.token, Purpose::SecondFactor, Some(user.id)).await?;
    let Some(pending) = pending else {
        let message = page.say("passkeys-expired", &[]);
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    };
    let pending: PasskeyAuthentication = from_json(pending)?;
    let checked = passkeys
        .webauthn
        .finish_passkey_authentication(&answer.credential, &pending);
    let result = match checked {
        Ok(result) => result,
        Err(error) => {
            let used = stored::by_credential_id(db, answer.credential.get_credential_id()).await?;
            refused(user.id, used.map(|p| p.id), &error);
            let message = page.say("passkeys-failed", &[]);
            return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
        }
    };
    // The challenge carries the passkeys the user had when it started;
    // one removed since then no longer counts.
    let found = stored::by_credential_id(db, result.cred_id().as_ref())
        .await?
        .filter(|p| p.user_id == user.id);
    let Some(found) = found else {
        refused(user.id, None, &WebauthnError::CredentialNotFound);
        let message = page.say("passkeys-unknown", &[]);
        return Ok(refuse(StatusCode::UNPROCESSABLE_ENTITY, message));
    };
    let mut passkey: Passkey = from_json(found.credential.clone())?;
    passkey.update_credential(&result);
    stored::used(db, found.id, &to_json(&passkey)?).await?;
    // Like a right code (or a recovery code): wrong codes before it no
    // longer count towards locking codes from the app.
    moekura_db::two_factor::clear_failures(db, user.id).await?;
    moekura_db::two_factor::end_challenge(db, &login).await?;
    let jar = jar.remove(crate::two_factor::challenge_cookie(
        &state,
        String::new(),
        time::Duration::ZERO,
    ));
    let jar = auth::log_in(&state, jar, &info, &user).await?;
    tracing::info!(
        user_id = user.id,
        passkey_id = found.id,
        "logged in with a passkey after the password"
    );
    let next = crate::account::safe_next(challenge.next.as_deref()).to_owned();
    Ok(go(flash::set(jar, Flash::LoggedIn), &next))
}

#[cfg(test)]
mod tests;
