//! Banning users and networks.

use axum::extract::Path;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use ipnet::IpNet;
use minijinja::{Value, context};
use moekura_core::moderation::{ActionKind, REASON_MAX_LEN};
use moekura_core::permissions::Permission;
use moekura_db::bans::{self, Ban};
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::users::{self, User};
use serde::Deserialize;
use time::{Duration, OffsetDateTime};

use crate::AppState;
use crate::auth::{CurrentUser, RequestInfo};
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

/// Ban lengths offered, in days; none means until lifted.
pub const DURATIONS: [(&str, Option<i64>); 6] = [
    ("1 day", Some(1)),
    ("3 days", Some(3)),
    ("1 week", Some(7)),
    ("1 month", Some(30)),
    ("1 year", Some(365)),
    ("Until lifted", None),
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/moderation/bans", get(index))
        .route("/users/{name}/ban", post(ban_user))
        .route("/users/{name}/unban", post(unban_user))
        .route("/moderation/ip-bans", post(ban_network_form))
        .route("/moderation/ip-bans/{id}/lift", post(lift_network_form))
}

#[derive(Debug, Deserialize)]
struct BanForm {
    #[serde(default)]
    reason: String,
    /// Days; empty for until lifted.
    #[serde(default)]
    days: String,
}

fn expiry(days: &str) -> Result<Option<OffsetDateTime>, AppError> {
    match days.trim() {
        "" => Ok(None),
        text => {
            let days: i64 = text
                .parse()
                .ok()
                .filter(|d| DURATIONS.iter().any(|(_, v)| *v == Some(*d)))
                .ok_or_else(|| AppError::BadRequest("Unknown ban length".into()))?;
            Ok(Some(OffsetDateTime::now_utc() + Duration::days(days)))
        }
    }
}

fn reason(text: &str) -> Result<&str, AppError> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > REASON_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "Give a reason of at most {REASON_MAX_LEN} characters"
        )));
    }
    Ok(text)
}

/// Whether `current` may ban `target`: `BanUsers`, and a higher rank.
pub fn may_ban(state: &AppState, current: &CurrentUser, target: &User) -> bool {
    let site = state.site.get();
    let target_rank = site.role(target.role_id).map_or(0, |r| r.rank);
    current.can(Permission::BanUsers)
        && current.user.as_ref().is_some_and(|me| me.id != target.id)
        && current.role.rank > target_rank
}

pub fn ban_context(ban: &Ban) -> Value {
    context! {
        user => ban.user_name,
        reason => ban.reason,
        by => ban.banner_name,
        since => ban.created_at.date().to_string(),
        until => ban.expires_at.map(|t| t.date().to_string()),
        lifted => ban.lifted_at.is_some(),
        active => ban.active,
    }
}

/// The user called `name`, if `current` may ban them.
async fn target(state: &AppState, current: &CurrentUser, name: &str) -> Result<User, AppError> {
    let user = users::by_name(state.db.primary(), name)
        .await?
        .ok_or(AppError::NotFound)?;
    if !may_ban(state, current, &user) {
        return Err(AppError::Forbidden);
    }
    Ok(user)
}

fn saved(jar: CookieJar, to: &str) -> Response {
    (flash::set(jar, Flash::Saved), Redirect::to(to)).into_response()
}

async fn ban_user(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(form): Form<BanForm>,
) -> Result<Response, AppError> {
    let expires_at = expiry(&form.days)?;
    let user = ban(page.state(), &page.current, &name, &form.reason, expires_at).await?;
    Ok(saved(jar, &format!("/users/{}", user.name)))
}

/// Bans the user called `name` until `expires_at` (or until lifted).
pub(crate) async fn ban(
    state: &AppState,
    current: &CurrentUser,
    name: &str,
    reason_text: &str,
    expires_at: Option<OffsetDateTime>,
) -> Result<User, AppError> {
    let user = target(state, current, name).await?;
    let reason = reason(reason_text)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    bans::ban(&mut *tx, user.id, reason, expires_at, actor).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::UserBan)
            .user(user.id)
            .reason(reason)
            .details(serde_json::json!({
                "until": expires_at.map(|t| t.date().to_string()),
            })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = user.name, "user banned");
    Ok(user)
}

async fn unban_user(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    let user = unban(page.state(), &page.current, &name).await?;
    Ok(saved(jar, &format!("/users/{}", user.name)))
}

/// Lifts the ban on the user called `name`.
pub(crate) async fn unban(
    state: &AppState,
    current: &CurrentUser,
    name: &str,
) -> Result<User, AppError> {
    let user = target(state, current, name).await?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    if bans::lift(&mut *tx, user.id, actor).await? == 0 {
        return Err(AppError::BadRequest("That user isn't banned".into()));
    }
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::UserUnban).user(user.id),
    )
    .await?;
    tx.commit().await?;
    Ok(user)
}

#[derive(Debug, Deserialize)]
struct NetworkForm {
    /// An address or CIDR range.
    #[serde(default)]
    network: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    days: String,
}

async fn ban_network_form(
    page: Page,
    jar: CookieJar,
    info: RequestInfo,
    Form(form): Form<NetworkForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::BanUsers)?;
    let expires_at = expiry(&form.days)?;
    ban_network(
        page.state(),
        &page.current,
        info.ip,
        &form.network,
        &form.reason,
        expires_at,
    )
    .await?;
    Ok(saved(jar, "/moderation/bans"))
}

/// Bans a network (an address or a CIDR range) from making changes.
/// `own_ip` is the requester's address, which the range may not include:
/// they couldn't lift the ban.
pub(crate) async fn ban_network(
    state: &AppState,
    current: &CurrentUser,
    own_ip: Option<std::net::IpAddr>,
    text: &str,
    reason_text: &str,
    expires_at: Option<OffsetDateTime>,
) -> Result<IpNet, AppError> {
    current.require(Permission::BanUsers)?;
    let text = text.trim();
    let network: IpNet = text
        .parse()
        .or_else(|_| text.parse::<std::net::IpAddr>().map(IpNet::from))
        .map_err(|_| {
            AppError::BadRequest("Give an address or a range like 203.0.113.0/24".into())
        })?;
    // Guard against banning everyone by mistake.
    if network.prefix_len() < if network.addr().is_ipv4() { 8 } else { 16 } {
        return Err(AppError::BadRequest("That range is too wide".into()));
    }
    if own_ip.is_some_and(|ip| network.contains(&ip)) {
        return Err(AppError::BadRequest(
            "That range includes your own address".into(),
        ));
    }
    let reason = reason(reason_text)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    bans::ban_network(&mut *tx, network, reason, expires_at, actor).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::IpBan)
            .reason(reason)
            .details(serde_json::json!({ "network": network.trunc().to_string() })),
    )
    .await?;
    tx.commit().await?;
    Ok(network.trunc())
}

async fn lift_network_form(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    lift_network(page.state(), &page.current, id).await?;
    Ok(saved(jar, "/moderation/bans"))
}

/// Lifts network ban `id`.
pub(crate) async fn lift_network(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<(), AppError> {
    current.require(Permission::BanUsers)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    let network = bans::lift_network(&mut *tx, id)
        .await?
        .ok_or(AppError::NotFound)?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::IpUnban)
            .details(serde_json::json!({ "network": network.to_string() })),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn index(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::BanUsers)?;
    let db = page.state().db.primary();
    let users = bans::active(db, 200).await?;
    let networks = bans::active_networks(db).await?;
    Ok(page.render(
        "moderation_bans.html",
        context! {
            bans => users.iter().map(ban_context).collect::<Vec<_>>(),
            networks => networks.iter().map(|n| context! {
                id => n.id,
                network => n.network.to_string(),
                reason => n.reason,
                by => n.banner_name,
                until => n.expires_at.map(|t| t.date().to_string()),
            }).collect::<Vec<_>>(),
            durations => durations(),
        },
    ))
}

pub fn durations() -> Vec<Value> {
    DURATIONS
        .iter()
        .map(|(label, days)| context! { label => label, days => days })
        .collect()
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn banned_users_can_look_but_not_act(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            super::routes()
                .merge(crate::users::routes())
                .merge(crate::posts::routes())
                .merge(crate::upload::routes(max)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;

        // Members can't ban; moderators can't ban admins.
        assert_eq!(
            app.post_form("/users/mod/ban", Some(&alice), &[], "reason=x")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.post_form("/users/root/ban", Some(&moderator), &[], "reason=x")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let profile = app.get("/users/alice", Some(&moderator)).await.body;
        assert!(profile.contains("/users/alice/ban"), "{profile}");
        let response = app
            .post_form(
                "/users/alice/ban",
                Some(&moderator),
                &[],
                "reason=spam&days=7",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);

        let home = app.get("/", Some(&alice)).await;
        assert_eq!(home.status, StatusCode::OK);
        assert!(home.body.contains("You are banned until"), "{}", home.body);
        assert!(home.body.contains("spam"));
        let upload = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &[("rating", "g".to_owned())],
                Some(("a.png", &fixture::png(20, 20))),
            )
            .await;
        assert_eq!(upload.status, StatusCode::FORBIDDEN);
        let bans = app.get("/moderation/bans", Some(&admin)).await.body;
        assert!(bans.contains("alice") && bans.contains("spam"), "{bans}");

        app.post("/users/alice/unban", Some(&admin), &[]).await;
        assert!(
            !app.get("/", Some(&alice))
                .await
                .body
                .contains("You are banned")
        );
        let history = app.get("/users/alice", Some(&moderator)).await.body;
        assert!(history.contains("lifted"), "{history}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn network_bans_block_changes(pool: PgPool) {
        let state = test_state(&pool).await;
        let routes = || super::routes().merge(crate::account::routes());
        let moderator_at: std::net::SocketAddr = "192.0.2.10:4000".parse().unwrap();
        let visitor_at: std::net::SocketAddr = "198.51.100.23:4000".parse().unwrap();
        let moderation = TestApp::with_peer(state.clone(), routes(), moderator_at);
        let visitor = TestApp::with_peer(state, routes(), visitor_at);
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        for bad in [
            "network=0.0.0.0%2F0&reason=x",
            "network=nonsense&reason=x",
            "network=198.51.100.0%2F24",
            "network=192.0.2.0%2F24&reason=oops",
        ] {
            let response = moderation
                .post_form("/moderation/ip-bans", Some(&admin), &[], bad)
                .await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST, "{bad}");
        }
        let response = moderation
            .post_form(
                "/moderation/ip-bans",
                Some(&admin),
                &[],
                "network=198.51.100.0%2F24&reason=abuse&days=",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);

        // Everything from that network that changes things is refused,
        // with the reason; looking still works.
        let register = visitor.post_form("/register", None, &[], "name=x").await;
        assert_eq!(register.status, StatusCode::FORBIDDEN);
        assert!(register.body.contains("abuse"), "{}", register.body);
        assert_eq!(visitor.get("/login", None).await.status, StatusCode::OK);

        let id: i64 = sqlx::query_scalar("SELECT id FROM ip_bans")
            .fetch_one(&pool)
            .await
            .unwrap();
        let response = moderation
            .post(&format!("/moderation/ip-bans/{id}/lift"), Some(&admin), &[])
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        let register = visitor.post_form("/register", None, &[], "name=x").await;
        assert_ne!(register.status, StatusCode::FORBIDDEN);
    }
}
