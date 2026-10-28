//! Banning users and networks.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use ipnet::IpNet;
use minijinja::{Value, context};
use moekura_core::moderation::{ActionKind, MAX_BAN_DAYS, REASON_MAX_LEN};
use moekura_core::permissions::{PARTIAL_BAN, Permission, Permissions};
use moekura_db::ban_appeals;
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
        .route("/bans/appeal", get(appeal_page).post(appeal))
        .route(
            "/moderation/ban-appeals/{id}/{decision}",
            post(decide_appeal_form),
        )
}

#[derive(Debug, Deserialize)]
struct BanForm {
    #[serde(default)]
    reason: String,
    /// Days; empty for until lifted.
    #[serde(default)]
    days: String,
    /// `partial` to take away only what `restrict` names; anything else
    /// is a full ban.
    #[serde(default)]
    kind: String,
    /// Permission keys a partial ban takes away.
    #[serde(default)]
    restrict: Vec<String>,
}

/// What a partial ban takes away, from permission keys; an error if none,
/// or one a partial ban can't take.
pub(crate) fn restrictions(keys: &[String]) -> Result<Permissions, AppError> {
    let mut taken = Permissions::NONE;
    for key in keys {
        let permission = PARTIAL_BAN
            .into_iter()
            .find(|p| p.key() == key)
            .ok_or_else(|| AppError::BadRequest(format!("A ban can't take away `{key}` alone")))?;
        taken = taken.with(Permissions::of(&[permission]));
    }
    if taken == Permissions::NONE {
        return Err(AppError::BadRequest(
            "Tick what the ban takes away, or ban them from everything".into(),
        ));
    }
    Ok(taken)
}

/// What a partial ban's `bits` take away, by key.
pub(crate) fn restricted_keys(bits: i64) -> Vec<&'static str> {
    Permissions::from_db(bits).iter().map(|p| p.key()).collect()
}

/// Where to go after banning or unbanning: the profile, unless the form
/// came from elsewhere (the user moderation page).
#[derive(Debug, Default, Deserialize)]
struct Back {
    back: Option<String>,
}

impl Back {
    fn or_profile(&self, user: &User) -> String {
        match &self.back {
            Some(back) => crate::account::safe_next(Some(back)).to_owned(),
            None => format!(
                "/users/{}",
                url::form_urlencoded::byte_serialize(user.name.as_bytes()).collect::<String>()
            ),
        }
    }
}

/// When a ban of `days` days made now ends; `None` days is until lifted.
/// The web and the API share it; the error says what's wrong.
pub(crate) fn expiry(days: Option<i64>) -> Result<Option<OffsetDateTime>, String> {
    match days {
        None => Ok(None),
        Some(days @ 1..=MAX_BAN_DAYS) => Ok(Some(OffsetDateTime::now_utc() + Duration::days(days))),
        Some(_) => Err(format!(
            "A ban lasts from 1 to {MAX_BAN_DAYS} days, or until lifted"
        )),
    }
}

/// [`expiry`] for a form field: days, or empty for until lifted.
fn form_expiry(days: &str) -> Result<Option<OffsetDateTime>, AppError> {
    let days = match days.trim() {
        "" => None,
        text => Some(
            text.parse()
                .map_err(|_| AppError::BadRequest("Unknown ban length".into()))?,
        ),
    };
    expiry(days).map_err(AppError::BadRequest)
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
    current.can(Permission::BanUsers) && crate::ranks::outranks(&state.site.get(), current, target)
}

/// Whether `current` may lift or replace bans set by `banners` (`None`
/// for an account since deleted): their own, or those of someone they
/// outrank, so nobody undoes what those above them decided.
pub(crate) async fn may_change_bans(
    state: &AppState,
    current: &CurrentUser,
    banners: &[Option<i64>],
) -> Result<bool, AppError> {
    let me = current.user.as_ref().map(|u| u.id);
    let others: Vec<i64> = banners
        .iter()
        .flatten()
        .copied()
        .filter(|id| Some(*id) != me)
        .collect();
    if others.is_empty() {
        return Ok(true);
    }
    let site = state.site.get();
    let found = users::by_ids(state.db.primary(), &others).await?;
    Ok(found
        .iter()
        .all(|banner| crate::ranks::outranks(&site, current, banner)))
}

/// Refuses unless `current` may change the bans on `user_id` in force;
/// call it in the transaction changing them.
async fn check_banners(
    state: &AppState,
    current: &CurrentUser,
    conn: &mut sqlx::PgConnection,
    user_id: i64,
) -> Result<(), AppError> {
    let banners = bans::banners_in_force(conn, user_id).await?;
    if may_change_bans(state, current, &banners).await? {
        Ok(())
    } else {
        Err(AppError::Blocked(
            "Someone ranked at or above you set this ban; only they or someone above them can lift or change it"
                .into(),
        ))
    }
}

pub fn ban_context(ban: &Ban) -> Value {
    context! {
        user => ban.user_name,
        reason => ban.reason,
        restricts => ban.restricts.map(restricted_keys),
        by => ban.banner_name,
        since => crate::dates::day(ban.created_at),
        until => ban.expires_at.map(crate::dates::day),
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
    Query(back): Query<Back>,
    axum_extra::extract::Form(form): axum_extra::extract::Form<BanForm>,
) -> Result<Response, AppError> {
    let expires_at = form_expiry(&form.days)?;
    let restricts = match form.kind.as_str() {
        "partial" => Some(restrictions(&form.restrict)?),
        _ => None,
    };
    let user = ban(
        page.state(),
        &page.current,
        &name,
        &form.reason,
        expires_at,
        restricts,
    )
    .await?;
    Ok(saved(jar, &back.or_profile(&user)))
}

/// Bans the user called `name` until `expires_at` (or until lifted), from
/// everything but looking around or, with `restricts`, only from that,
/// replacing any ban they're under.
pub(crate) async fn ban(
    state: &AppState,
    current: &CurrentUser,
    name: &str,
    reason_text: &str,
    expires_at: Option<OffsetDateTime>,
    restricts: Option<Permissions>,
) -> Result<User, AppError> {
    let user = target(state, current, name).await?;
    let reason = reason(reason_text)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    check_banners(state, current, &mut tx, user.id).await?;
    let bits = restricts.map(Permissions::to_db);
    let replaced = bans::ban(&mut tx, user.id, reason, expires_at, bits, actor).await?;
    let mut details = serde_json::json!({
        "until": expires_at.map(|t| t.date().to_string()),
    });
    if let Some(bits) = bits {
        details["restricts"] = restricted_keys(bits).into();
    }
    if let Some(replaced) = replaced {
        details["replaced_until"] = replaced.expires_at.map(|t| t.date().to_string()).into();
    }
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::UserBan)
            .user(user.id)
            .reason(reason)
            .details(details),
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
    Query(back): Query<Back>,
) -> Result<Response, AppError> {
    let user = unban(page.state(), &page.current, &name).await?;
    Ok(saved(jar, &back.or_profile(&user)))
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
    check_banners(state, current, &mut tx, user.id).await?;
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
    /// `full` to keep the network from seeing the site at all; otherwise
    /// it can't change anything.
    #[serde(default)]
    kind: String,
}

async fn ban_network_form(
    page: Page,
    jar: CookieJar,
    info: RequestInfo,
    Form(form): Form<NetworkForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::BanUsers)?;
    let expires_at = form_expiry(&form.days)?;
    let full = match form.kind.as_str() {
        "" | "partial" => false,
        "full" => true,
        _ => return Err(AppError::BadRequest("Unknown kind of ban".into())),
    };
    ban_network(
        page.state(),
        &page.current,
        info.ip,
        &form.network,
        &form.reason,
        expires_at,
        full,
    )
    .await?;
    Ok(saved(jar, "/moderation/bans"))
}

/// Refuses to ban `network`, or lift a ban on it, when an account
/// `current` doesn't outrank (themselves included) was seen on it, as
/// user bans are refused: that would act on them.
async fn check_rank_in(
    state: &AppState,
    current: &CurrentUser,
    network: IpNet,
) -> Result<(), AppError> {
    let top = moekura_db::user_ips::top_rank_in(state.db.primary(), network).await?;
    if top.is_some_and(|rank| rank >= current.standing().as_rank()) {
        return Err(AppError::Blocked(
            "That range includes an address of someone ranked at or above you".into(),
        ));
    }
    Ok(())
}

/// Whether a full ban of `network` needs `ManageUsers`: wider than a
/// /24 (IPv4) or /48 (IPv6), it shuts out whole providers.
fn is_wide(network: IpNet) -> bool {
    network.prefix_len() < if network.addr().is_ipv4() { 24 } else { 48 }
}

/// Whether any ban of `network` needs `ManageUsers`: wider than a /16
/// (IPv4) or /32 (IPv6), it reaches far beyond one household or office.
fn is_very_wide(network: IpNet) -> bool {
    network.prefix_len() < if network.addr().is_ipv4() { 16 } else { 32 }
}

/// What a typed address or range bans: a range as given, an IPv4
/// address alone, and an IPv6 address's /64, which one household or
/// phone usually has to itself and moves around in.
fn network_from(text: &str) -> Option<IpNet> {
    let network = match text.parse::<IpNet>() {
        Ok(network) => network,
        Err(_) => match text.parse::<std::net::IpAddr>().ok()? {
            ip @ std::net::IpAddr::V4(_) => IpNet::from(ip),
            ip @ std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(_) => IpNet::from(ip),
                None => IpNet::new(ip, 64).ok()?.trunc(),
            },
        },
    };
    Some(crate::client_ip::canonical_net(network))
}

/// Bans a network (an address or a CIDR range) from making changes, or
/// with `full` from seeing the site at all. `own_ip` is the requester's
/// address, which the range may not include: they couldn't lift the ban.
/// Nor may it include an address of anyone ranked at or above them.
pub(crate) async fn ban_network(
    state: &AppState,
    current: &CurrentUser,
    own_ip: Option<std::net::IpAddr>,
    text: &str,
    reason_text: &str,
    expires_at: Option<OffsetDateTime>,
    full: bool,
) -> Result<IpNet, AppError> {
    current.require(Permission::BanUsers)?;
    let network = network_from(text.trim()).ok_or_else(|| {
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
    if is_very_wide(network) && !current.can(Permission::ManageUsers) {
        return Err(AppError::BadRequest(
            "Only staff who manage users can ban a range wider than /16 (/32 for IPv6)".into(),
        ));
    }
    if full && is_wide(network) && !current.can(Permission::ManageUsers) {
        return Err(AppError::BadRequest(
            "Only staff who manage users can fully ban a range wider than /24 (/48 for IPv6)"
                .into(),
        ));
    }
    // Last, so that only a ban actually made tells who was seen there.
    check_rank_in(state, current, network).await?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    bans::ban_network(&mut tx, network, reason, expires_at, full, actor).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::IpBan)
            .reason(reason)
            .details(serde_json::json!({
                "network": network.trunc().to_string(),
                "kind": if full { "full" } else { "partial" },
            })),
    )
    .await?;
    tx.commit().await?;
    // Other nodes reload when notified; this one needn't wait.
    state.site.reload(state.db.primary()).await?;
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

/// Lifts network ban `id`, unless it covers an address of anyone ranked
/// at or above `current`: that's for someone above them, or for
/// `moekura admin lift-network-ban`.
pub(crate) async fn lift_network(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<(), AppError> {
    current.require(Permission::BanUsers)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    let network = bans::lock_network(&mut tx, id)
        .await?
        .ok_or(AppError::NotFound)?;
    check_rank_in(state, current, network).await?;
    let network = bans::lift_network(&mut tx, id, actor)
        .await?
        .ok_or(AppError::NotFound)?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::IpUnban)
            .details(serde_json::json!({ "network": network.to_string() })),
    )
    .await?;
    tx.commit().await?;
    state.site.reload(state.db.primary()).await?;
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    /// Fills in the form to ban a network.
    #[serde(default)]
    network: String,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::BanUsers)?;
    let state = page.state();
    let db = state.db.primary();
    let users = bans::active(db, 200).await?;
    let networks = bans::active_networks(db).await?;
    let mut appeals = Vec::new();
    for a in ban_appeals::open(db, 200).await? {
        let user = users::by_id(db, a.user_id).await?;
        // Those they couldn't decide, they see but can't act on.
        let can_decide = match &user {
            Some(user) => {
                may_ban(state, &page.current, user)
                    && may_change_bans(state, &page.current, &[a.banner_id]).await?
            }
            None => false,
        };
        appeals.push(context! {
            id => a.id,
            user => a.user_name,
            reason => a.reason,
            when => crate::dates::day(a.created_at),
            ban_reason => a.ban_reason,
            until => a.ban_expires_at.map(crate::dates::day),
            by => a.banner_name,
            can_decide => can_decide,
        });
    }
    Ok(page.render(
        "moderation_bans.html",
        context! {
            bans => users.iter().map(ban_context).collect::<Vec<_>>(),
            networks => networks.iter().map(|n| context! {
                id => n.id,
                network => n.network.to_string(),
                reason => n.reason,
                by => n.banner_name,
                until => n.expires_at.map(crate::dates::day),
                full => n.full,
            }).collect::<Vec<_>>(),
            durations => durations(),
            network => query.network,
            appeals => appeals,
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct AppealForm {
    #[serde(default)]
    reason: String,
}

/// A banned user's own page for appealing their ban.
async fn appeal_page(page: Page) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = page.state().db.primary();
    let ban = bans::for_user(db, user.id)
        .await?
        .into_iter()
        .find(|b| b.active);
    let appeals = ban_appeals::for_user(db, user.id).await?;
    Ok(page.render(
        "ban_appeal.html",
        context! {
            ban => ban.as_ref().map(ban_context),
            open => appeals.iter().any(|a| a.status == "open"),
            appeals => appeals.iter().map(|a| context! {
                reason => a.reason,
                status => a.status,
                outcome => a.outcome,
                when => crate::dates::day(a.created_at),
            }).collect::<Vec<_>>(),
        },
    ))
}

async fn appeal(
    page: Page,
    jar: CookieJar,
    Form(form): Form<AppealForm>,
) -> Result<Response, AppError> {
    appeal_ban(page.state(), &page.current, &form.reason).await?;
    Ok(saved(jar, "/bans/appeal"))
}

/// Appeals `current`'s ban in force, asking staff to lift it.
pub(crate) async fn appeal_ban(
    state: &AppState,
    current: &CurrentUser,
    reason: &str,
) -> Result<i64, AppError> {
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > REASON_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "Say why, in at most {REASON_MAX_LEN} characters"
        )));
    }
    let refused = |e: ban_appeals::AppealError| match e {
        ban_appeals::AppealError::Db(e) => AppError::from(e),
        other => AppError::BadRequest(other.to_string()),
    };
    // Before the rate limit, so a refused repeat doesn't use it up.
    let mut conn = state.db.primary().acquire().await?;
    ban_appeals::check_new(&mut conn, user.id)
        .await
        .map_err(refused)?;
    drop(conn);
    state.rate_limits.check_appeal(user.id).await?;
    let mut tx = state.db.primary().begin().await?;
    let id = ban_appeals::create(&mut tx, user.id, reason)
        .await
        .map_err(refused)?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(Some(user.id), ActionKind::BanAppealCreate)
            .user(user.id)
            .reason(reason)
            .details(serde_json::json!({ "appeal": id })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = user.name, "ban appealed");
    Ok(id)
}

#[derive(Debug, Default, Deserialize)]
struct DecisionForm {
    /// How it ended, for the user and the log.
    #[serde(default)]
    outcome: String,
}

async fn decide_appeal_form(
    page: Page,
    jar: CookieJar,
    Path((id, decision)): Path<(i64, String)>,
    Form(form): Form<DecisionForm>,
) -> Result<Response, AppError> {
    let grant = match decision.as_str() {
        "grant" => true,
        "deny" => false,
        _ => return Err(AppError::NotFound),
    };
    decide_appeal(page.state(), &page.current, id, grant, &form.outcome).await?;
    Ok(saved(jar, "/moderation/bans"))
}

/// Grants ban appeal `id` (lifting the ban) or turns it down, with
/// `outcome`, as `current`, who must be able to lift that ban.
pub(crate) async fn decide_appeal(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    grant: bool,
    outcome: &str,
) -> Result<(), AppError> {
    current.require(Permission::BanUsers)?;
    let outcome = crate::moderation::check_reason(outcome)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let db = state.db.primary();
    let mut tx = db.begin().await?;
    let appeal = ban_appeals::lock(&mut tx, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let user = users::by_id(db, appeal.user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !may_ban(state, current, &user) {
        return Err(AppError::Forbidden);
    }
    check_banners(state, current, &mut tx, user.id).await?;
    if !ban_appeals::resolve(&mut *tx, id, grant, actor, outcome).await? {
        return Err(AppError::BadRequest(
            "The appeal was decided already".into(),
        ));
    }
    if grant {
        bans::lift(&mut *tx, user.id, actor).await?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(actor, ActionKind::UserUnban)
                .user(user.id)
                .details(serde_json::json!({ "appeal": id })),
        )
        .await?;
    }
    let kind = if grant {
        ActionKind::BanAppealGrant
    } else {
        ActionKind::BanAppealDeny
    };
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, kind)
            .user(user.id)
            .reason(outcome)
            .details(serde_json::json!({ "appeal": id })),
    )
    .await?;
    tx.commit().await?;
    let told = match (grant, outcome) {
        (true, _) => "Your ban appeal was granted: the ban is lifted".to_owned(),
        (false, "") => "Your ban appeal was turned down".to_owned(),
        (false, why) => format!("Your ban appeal was turned down: “{why}”"),
    };
    crate::notifications::moderated(state, Some(user.id), actor, &told, "/bans/appeal").await;
    Ok(())
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
    async fn banned_users_get_nothing_visitors_were_given(pool: PgPool) {
        use moekura_core::permissions::{Permission, Permissions};
        // Visitors may comment and vote here.
        let visitors =
            Permissions::of(&[Permission::ViewPosts, Permission::Comment, Permission::Vote]);
        sqlx::query("UPDATE roles SET permissions = $1 WHERE system_key = 'anonymous'")
            .bind(visitors.to_db())
            .execute(&pool)
            .await
            .unwrap();
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let state = test_state(&pool).await;
        let current = crate::test_support::current_user(&state, &alice).await;
        let banner = crate::test_support::current_user(&state, &moderator).await;
        super::ban(&state, &banner, "alice", "spam", None, None)
            .await
            .unwrap();
        assert!(current.can(Permission::Comment), "before the ban");
        let banned = crate::test_support::current_user(&state, &alice).await;
        assert!(banned.can(Permission::ViewPosts));
        for permission in [Permission::Comment, Permission::Vote, Permission::Upload] {
            assert!(!banned.can(permission), "{permission:?}");
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn partial_bans_take_away_only_what_they_name(pool: PgPool) {
        use moekura_core::permissions::Permission;
        let state = test_state(&pool).await;
        let app = TestApp::new(state.clone(), super::routes().merge(crate::users::routes()));
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let refused = app
            .post_form(
                "/users/alice/ban",
                Some(&moderator),
                &[],
                "reason=x&kind=partial",
            )
            .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "nothing ticked");
        let banned = app
            .post_form(
                "/users/alice/ban",
                Some(&moderator),
                &[],
                "reason=rude+comments&days=7&kind=partial&restrict=comment&restrict=send_messages",
            )
            .await;
        assert_eq!(banned.status, StatusCode::SEE_OTHER, "{}", banned.body);
        let current = crate::test_support::current_user(&state, &alice).await;
        assert!(!current.can(Permission::Comment));
        assert!(!current.can(Permission::SendMessages));
        assert!(current.can(Permission::Upload) && current.can(Permission::Vote));
        let home = app.get("/users/alice", Some(&alice)).await.body;
        assert!(home.contains("You are banned from some things"), "{home}");
        assert!(home.contains("Send private messages"), "{home}");
        let logged: serde_json::Value =
            sqlx::query_scalar("SELECT details FROM mod_actions WHERE action = 'user.ban'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            logged["restricts"],
            serde_json::json!(["comment", "send_messages"])
        );
        // A partial ban can't take what isn't one of its choices.
        let odd = app
            .post_form(
                "/users/alice/ban",
                Some(&moderator),
                &[],
                "reason=x&kind=partial&restrict=view_posts",
            )
            .await;
        assert_eq!(odd.status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn banned_users_appeal(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let other = session_for(&pool, "mod2", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let send = async |reason: &str| {
            app.post_form(
                "/bans/appeal",
                Some(&alice),
                &[],
                &format!("reason={reason}"),
            )
            .await
            .status
        };
        assert_eq!(send("why").await, StatusCode::BAD_REQUEST, "not banned");
        app.post_form("/users/alice/ban", Some(&moderator), &[], "reason=spam")
            .await;
        let page = app.get("/bans/appeal", Some(&alice)).await.body;
        assert!(page.contains("“spam”"), "{page}");
        assert_eq!(send("it+was+my+brother").await, StatusCode::SEE_OTHER);
        assert_eq!(
            send("again").await,
            StatusCode::BAD_REQUEST,
            "one at a time"
        );

        // Only those who could lift the ban decide it.
        let id: i64 = sqlx::query_scalar("SELECT id FROM ban_appeals")
            .fetch_one(&pool)
            .await
            .unwrap();
        let queue = app.get("/moderation/bans", Some(&other)).await.body;
        assert!(queue.contains("it was my brother"), "{queue}");
        assert!(!queue.contains(&format!("/moderation/ban-appeals/{id}/grant")));
        let refused = app
            .post_form(
                &format!("/moderation/ban-appeals/{id}/grant"),
                Some(&other),
                &[],
                "",
            )
            .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        let denied = app
            .post_form(
                &format!("/moderation/ban-appeals/{id}/deny"),
                Some(&moderator),
                &[],
                "outcome=no+brothers+here",
            )
            .await;
        assert_eq!(denied.status, StatusCode::SEE_OTHER);
        assert_eq!(
            send("please").await,
            StatusCode::BAD_REQUEST,
            "a week first"
        );
        sqlx::query("UPDATE ban_appeals SET resolved_at = now() - interval '8 days'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(send("please").await, StatusCode::SEE_OTHER);
        let id: i64 = sqlx::query_scalar("SELECT max(id) FROM ban_appeals")
            .fetch_one(&pool)
            .await
            .unwrap();
        let granted = app
            .post_form(
                &format!("/moderation/ban-appeals/{id}/grant"),
                Some(&admin),
                &[],
                "",
            )
            .await;
        assert_eq!(granted.status, StatusCode::SEE_OTHER);
        let banned: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM bans WHERE lifted_at IS NULL)")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!banned, "lifted");
        let told: Vec<String> = sqlx::query_scalar(
            "SELECT subject FROM notifications WHERE kind = 'moderation' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            told,
            [
                "Your ban appeal was turned down: “no brothers here”",
                "Your ban appeal was granted: the ban is lifted"
            ]
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn bans_from_above_stay(pool: PgPool) {
        let state = test_state(&pool).await;
        let app = TestApp::new(state, super::routes().merge(crate::users::routes()));
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let other = session_for(&pool, "mod2", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        session_for(&pool, "alice", SystemRole::Member).await;
        session_for(&pool, "bob", SystemRole::Member).await;
        let banned_until = || async {
            sqlx::query_scalar::<_, Option<time::OffsetDateTime>>(
                "SELECT b.expires_at FROM bans b JOIN users u ON u.id = b.user_id
                 WHERE u.name = 'alice' AND b.lifted_at IS NULL",
            )
            .fetch_optional(&pool)
            .await
            .unwrap()
        };

        let response = app
            .post_form("/users/alice/ban", Some(&admin), &[], "reason=spam")
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        // A moderator can neither lift an admin's ban nor shorten it.
        let lift = app.post("/users/alice/unban", Some(&moderator), &[]).await;
        assert_eq!(lift.status, StatusCode::FORBIDDEN, "{}", lift.body);
        let shorten = app
            .post_form("/users/alice/ban", Some(&moderator), &[], "reason=x&days=1")
            .await;
        assert_eq!(shorten.status, StatusCode::FORBIDDEN);
        assert_eq!(banned_until().await, Some(None), "still until lifted");
        let profile = app.get("/users/alice", Some(&moderator)).await.body;
        assert!(!profile.contains("/users/alice/unban"), "{profile}");
        assert!(
            profile.contains("only they or someone above them"),
            "{profile}"
        );
        assert!(
            app.get("/users/alice", Some(&admin))
                .await
                .body
                .contains("/users/alice/unban")
        );

        // A peer's ban is theirs too; one's own can change.
        app.post_form("/users/bob/ban", Some(&moderator), &[], "reason=x&days=7")
            .await;
        let peer = app.post("/users/bob/unban", Some(&other), &[]).await;
        assert_eq!(peer.status, StatusCode::FORBIDDEN);
        let own = app.post("/users/bob/unban", Some(&moderator), &[]).await;
        assert_eq!(own.status, StatusCode::SEE_OTHER);
        let above = app.post("/users/alice/unban", Some(&admin), &[]).await;
        assert_eq!(above.status, StatusCode::SEE_OTHER);
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

        // A full ban keeps the network from seeing anything.
        let response = moderation
            .post_form(
                "/moderation/ip-bans",
                Some(&admin),
                &[],
                "network=198.51.100.0%2F24&reason=scraping&kind=full",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let look = visitor.get("/login", None).await;
        assert_eq!(look.status, StatusCode::FORBIDDEN);
        assert!(look.body.contains("scraping"), "{}", look.body);
        assert_eq!(
            moderation.get("/login", None).await.status,
            StatusCode::OK,
            "other networks are fine"
        );
        let page = moderation.get("/moderation/bans", Some(&admin)).await.body;
        assert!(page.contains("full"), "{page}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn ipv4_mapped_networks_ban_ipv4_clients(pool: PgPool) {
        let state = test_state(&pool).await;
        let routes = || super::routes().merge(crate::account::routes());
        let moderator_at: std::net::SocketAddr = "192.0.2.10:4000".parse().unwrap();
        let visitor_at: std::net::SocketAddr = "[::ffff:198.51.100.23]:4000".parse().unwrap();
        let moderation = TestApp::with_peer(state.clone(), routes(), moderator_at);
        let visitor = TestApp::with_peer(state, routes(), visitor_at);
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        // As a dual-stack server's logs show IPv4 clients: all of IPv4 is
        // too wide, and the moderator's own address is still theirs.
        for bad in ["::ffff:0:0%2F96", "::ffff:192.0.2.10"] {
            let response = moderation
                .post_form(
                    "/moderation/ip-bans",
                    Some(&admin),
                    &[],
                    &format!("network={bad}&reason=x"),
                )
                .await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST, "{bad}");
        }
        let response = moderation
            .post_form(
                "/moderation/ip-bans",
                Some(&admin),
                &[],
                "network=::ffff:198.51.100.0%2F120&reason=abuse",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let stored: String = sqlx::query_scalar("SELECT network::text FROM ip_bans")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, "198.51.100.0/24");
        let register = visitor.post_form("/register", None, &[], "name=x").await;
        assert_eq!(register.status, StatusCode::FORBIDDEN, "{}", register.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn network_bans_respect_rank(pool: PgPool) {
        let state = test_state(&pool).await;
        let routes = super::routes().merge(crate::user_moderation::routes());
        let app = TestApp::with_peer(state, routes, "192.0.2.10:4000".parse().unwrap());
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        session_for(&pool, "alice", SystemRole::Member).await;
        session_for(&pool, "bob", SystemRole::Member).await;
        for (name, ip) in [
            ("root", "198.51.100.7"),
            ("root", "203.0.113.5"),
            ("alice", "198.51.100.8"),
            ("alice", "203.0.113.5"),
            ("bob", "203.0.113.5"),
        ] {
            let id = moekura_db::users::by_name(&pool, name)
                .await
                .unwrap()
                .unwrap()
                .id;
            moekura_db::user_ips::record(&pool, id, ip.parse().unwrap())
                .await
                .unwrap();
        }
        let ban = async |who: &str, form: &str| {
            app.post_form("/moderation/ip-bans", Some(who), &[], form)
                .await
                .status
        };

        // Not a range an admin was seen on, by anyone who doesn't outrank
        // them; a member's is fine.
        for range in ["198.51.100.0%2F24", "198.51.100.7"] {
            let form = format!("network={range}&reason=x");
            assert_eq!(ban(&moderator, &form).await, StatusCode::FORBIDDEN);
            assert_eq!(ban(&admin, &form).await, StatusCode::FORBIDDEN);
        }
        assert_eq!(
            ban(&moderator, "network=198.51.100.8&reason=x").await,
            StatusCode::SEE_OTHER
        );
        // Invalid forms say so before anything is looked up.
        assert_eq!(
            ban(&moderator, "network=198.51.100.0%2F24").await,
            StatusCode::BAD_REQUEST
        );

        // So are very wide partial ones.
        assert_eq!(
            ban(&moderator, "network=10.0.0.0%2F12&reason=x").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ban(&admin, "network=172.16.0.0%2F12&reason=x").await,
            StatusCode::SEE_OTHER
        );
        // A bare IPv6 address bans the /64 its owner moves around in.
        assert_eq!(
            ban(&moderator, "network=2001%3Adb8%3A1%3A2%3A%3A9&reason=x").await,
            StatusCode::SEE_OTHER
        );
        let stored: Vec<String> =
            sqlx::query_scalar("SELECT network::text FROM ip_bans WHERE family(network) = 6")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(stored, ["2001:db8:1:2::/64"]);

        // Wide full bans are for those who manage users.
        assert_eq!(
            ban(&moderator, "network=10.0.0.0%2F16&reason=x&kind=full").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ban(&moderator, "network=10.0.0.0%2F16&reason=x").await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            ban(&moderator, "network=10.1.0.0%2F24&reason=x&kind=full").await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            ban(&admin, "network=10.2.0.0%2F16&reason=x&kind=full").await,
            StatusCode::SEE_OTHER
        );

        // Nor lifting a ban that covers an admin's address.
        let mut conn = pool.acquire().await.unwrap();
        let covering = moekura_db::bans::ban_network(
            &mut conn,
            "198.51.100.0/24".parse().unwrap(),
            "x",
            None,
            true,
            None,
        )
        .await
        .unwrap();
        drop(conn);
        let lift = format!("/moderation/ip-bans/{covering}/lift");
        assert_eq!(
            app.post(&lift, Some(&moderator), &[]).await.status,
            StatusCode::FORBIDDEN
        );
        let alices: i64 =
            sqlx::query_scalar("SELECT id FROM ip_bans WHERE network = '198.51.100.8/32'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            app.post(
                &format!("/moderation/ip-bans/{alices}/lift"),
                Some(&moderator),
                &[]
            )
            .await
            .status,
            StatusCode::SEE_OTHER
        );

        // Records, with their addresses, are only of those below the
        // viewer, and so are the other accounts on them.
        let root = app.get("/moderation/users/root", Some(&moderator)).await;
        assert_eq!(root.status, StatusCode::FORBIDDEN);
        let alice = app
            .get("/moderation/users/alice", Some(&moderator))
            .await
            .body;
        assert!(alice.contains("<code>203.0.113.5</code>"), "{alice}");
        assert!(alice.contains("/moderation/users/bob"), "{alice}");
        assert!(!alice.contains("/moderation/users/root"), "{alice}");
    }
}
