//! Admin pages: site settings, users and roles.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::moderation::{ActionKind, REASON_MAX_LEN};
use moekura_core::permissions::{Permission, Permissions, Role, SystemRole};
use moekura_core::settings::{RegistrationMode, SiteSettings};
use moekura_core::uploads::UploadLimits;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::users::{self, UserStatus};
use moekura_db::{jobs, roles, settings};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

const USERS_PAGE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin", get(overview))
        .route("/admin/jobs/{id}/{action}", post(job_action))
        .route("/admin/settings", get(settings_form).post(save_settings))
        .route("/admin/users", get(user_list))
        .route("/admin/users/{name}", post(update_user))
        .route("/admin/roles", get(role_list))
        .route("/admin/roles/new", post(create_role))
        .route("/admin/roles/{id}", post(update_role))
        .route("/admin/roles/{id}/delete", post(delete_role))
}

fn saved(jar: CookieJar, to: &str) -> Response {
    (flash::set(jar, Flash::Saved), Redirect::to(to)).into_response()
}

fn actor(page: &Page) -> Option<i64> {
    page.current.user.as_ref().map(|u| u.id)
}

// ---- overview -------------------------------------------------------------

async fn overview(page: Page) -> Result<Response, AppError> {
    if !(page.current.can(Permission::ManageSettings) || page.current.can(Permission::ManageUsers))
    {
        page.current.require(Permission::ManageSettings)?;
    }
    let db = page.state().db.primary();
    let stats = moekura_db::stats::overview(db).await?;
    let counts = jobs::counts_by_kind(db).await?;
    let dead = jobs::dead(db, 50).await?;
    let human = crate::posts::human_size;
    Ok(page.render(
        "admin_overview.html",
        context! {
            stats => context! {
                active_posts => stats.active_posts,
                pending_posts => stats.pending_posts,
                flagged_posts => stats.flagged_posts,
                deleted_posts => stats.deleted_posts,
                users => stats.users,
                tags => stats.tags,
                files => human(stats.file_bytes),
                database => human(stats.database_bytes),
            },
            jobs => counts.iter().map(|(kind, status, n)| context! { kind => kind, status => status, count => n }).collect::<Vec<_>>(),
            dead => dead.iter().map(|job| context! {
                id => job.id,
                kind => job.kind,
                payload => job.payload.to_string(),
                attempts => job.attempts,
                error => job.last_error,
                created => job.created_at.date().to_string(),
            }).collect::<Vec<_>>(),
            can_manage_jobs => page.current.can(Permission::ManageSettings),
        },
    ))
}

async fn job_action(
    page: Page,
    jar: CookieJar,
    Path((id, action)): Path<(i64, String)>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let mut tx = page.state().db.primary().begin().await?;
    let (done, kind) = match action.as_str() {
        "retry" => (jobs::retry(&mut *tx, id).await?, ActionKind::JobRetry),
        "discard" => (jobs::discard(&mut *tx, id).await?, ActionKind::JobDiscard),
        _ => return Err(AppError::NotFound),
    };
    if !done {
        return Err(AppError::BadRequest("That job isn't dead any more".into()));
    }
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor(&page), kind).details(json!({ "job": id })),
    )
    .await?;
    tx.commit().await?;
    Ok(saved(jar, "/admin"))
}

// ---- site settings --------------------------------------------------------

fn mode_name(mode: RegistrationMode) -> &'static str {
    match mode {
        RegistrationMode::Open => "open",
        RegistrationMode::Invite => "invite",
        RegistrationMode::Approval => "approval",
        RegistrationMode::Closed => "closed",
    }
}

fn render_settings(
    page: &Page,
    current: &SiteSettings,
    categories: &[moekura_db::tags::Category],
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let thresholds: Vec<Value> = categories
        .iter()
        .map(|c| {
            context! {
                name => c.name,
                label => c.label,
                value => current.tagger.thresholds.get(&c.name),
            }
        })
        .collect();
    page.render_with_status(
        status,
        "admin_settings.html",
        context! {
            settings => context! {
                site_name => current.site_name,
                registration_mode => mode_name(current.registration_mode),
                email_verification => current.email_verification,
                upload_approval => current.upload_approval,
                upload_limit_scaling => current.upload_limit_scaling,
                auto_promotion => current.auto_promotion,
                preview_all_ratings => current.preview_all_ratings,
                promotion => context! {
                    uploads => current.promotion_rules.uploads,
                    edits => current.promotion_rules.edits,
                    account_days => current.promotion_rules.account_days,
                    max_recent_deletions => current.promotion_rules.max_recent_deletions,
                },
                default_blacklist => current.default_blacklist,
                ip_history_days => current.ip_history_days,
                tagger => context! {
                    thresholds => thresholds,
                    auto_apply => current.tagger.auto_apply,
                    auto_threshold => current.tagger.auto_threshold,
                    auto_rating => current.tagger.auto_rating,
                },
            },
            mail_enabled => page.state().config.mail.is_enabled(),
            tagger_enabled => page.state().config.tagger.enabled,
            tagger_account => page.state().config.tagger.account,
            modes => ["open", "invite", "approval", "closed"],
            error => error,
        },
    )
}

async fn settings_form(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let current = page.state().site.get().settings.clone();
    let categories = moekura_db::tags::categories(page.state().db.primary()).await?;
    Ok(render_settings(
        &page,
        &current,
        &categories,
        None,
        StatusCode::OK,
    ))
}

#[derive(Debug, Deserialize)]
struct SettingsForm {
    #[serde(default)]
    site_name: String,
    #[serde(default)]
    registration_mode: String,
    /// Present when ticked.
    email_verification: Option<String>,
    /// Present when ticked.
    upload_approval: Option<String>,
    /// Present when ticked.
    upload_limit_scaling: Option<String>,
    /// Present when ticked.
    auto_promotion: Option<String>,
    /// Present when ticked.
    preview_all_ratings: Option<String>,
    #[serde(default)]
    promotion_uploads: String,
    #[serde(default)]
    promotion_edits: String,
    #[serde(default)]
    promotion_account_days: String,
    #[serde(default)]
    promotion_max_recent_deletions: String,
    #[serde(default)]
    default_blacklist: String,
    ip_history_days: Option<String>,
    /// Present when ticked.
    tagger_auto_apply: Option<String>,
    tagger_auto_threshold: Option<String>,
    /// Present when ticked.
    tagger_auto_rating: Option<String>,
    /// `tagger_threshold_<category>` fields.
    #[serde(flatten)]
    rest: std::collections::HashMap<String, String>,
}

/// A number field as JSON: the number, or the text as typed so the
/// setting is refused as invalid.
fn number(text: &str) -> serde_json::Value {
    text.trim()
        .parse::<u32>()
        .map_or_else(|_| json!(text.trim()), |n| json!(n))
}

async fn save_settings(
    page: Page,
    jar: CookieJar,
    Form(form): Form<SettingsForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let db = state.db.primary();
    let before = settings::load(db).await?;
    let categories = moekura_db::tags::categories(db).await?;
    // Thresholds for the categories on the form; a blank one falls back
    // to general's.
    let mut thresholds: serde_json::Map<String, serde_json::Value> = before
        .tagger
        .thresholds
        .iter()
        .map(|(category, percent)| (category.clone(), json!(percent)))
        .collect();
    for category in &categories {
        let Some(text) = form
            .rest
            .get(&format!("tagger_threshold_{}", category.name))
        else {
            continue;
        };
        if text.trim().is_empty() {
            thresholds.remove(&category.name);
        } else {
            thresholds.insert(category.name.clone(), number(text));
        }
    }
    let auto_threshold = form
        .tagger_auto_threshold
        .as_deref()
        .map_or_else(|| json!(before.tagger.auto_threshold), number);
    let wanted = [
        ("site_name", json!(form.site_name.trim())),
        ("registration_mode", json!(form.registration_mode)),
        (
            "email_verification",
            json!(form.email_verification.is_some()),
        ),
        ("upload_approval", json!(form.upload_approval.is_some())),
        (
            "upload_limit_scaling",
            json!(form.upload_limit_scaling.is_some()),
        ),
        ("auto_promotion", json!(form.auto_promotion.is_some())),
        (
            "preview_all_ratings",
            json!(form.preview_all_ratings.is_some()),
        ),
        (
            "promotion_rules",
            json!({
                "uploads": number(&form.promotion_uploads),
                "edits": number(&form.promotion_edits),
                "account_days": number(&form.promotion_account_days),
                "max_recent_deletions": number(&form.promotion_max_recent_deletions),
            }),
        ),
        (
            "default_blacklist",
            json!(form.default_blacklist.replace("\r\n", "\n").trim()),
        ),
        (
            "ip_history_days",
            form.ip_history_days
                .as_deref()
                .map_or_else(|| json!(before.ip_history_days), number),
        ),
        (
            "tagger",
            json!({
                "thresholds": thresholds,
                "auto_apply": form.tagger_auto_apply.is_some(),
                "auto_threshold": auto_threshold,
                "auto_rating": form.tagger_auto_rating.is_some(),
            }),
        ),
    ];
    let current = before.to_map();
    // Validate everything first, so a bad field changes nothing.
    let mut checked = before.clone();
    for (key, value) in &wanted {
        match checked.with_value(key, value.clone()) {
            Ok(next) => checked = next,
            Err(error) => {
                let mut shown = checked.clone();
                shown.site_name = form.site_name.clone();
                shown.default_blacklist = form.default_blacklist.clone();
                return Ok(render_settings(
                    &page,
                    &shown,
                    &categories,
                    Some(error.to_string()),
                    StatusCode::UNPROCESSABLE_ENTITY,
                ));
            }
        }
    }
    // All or nothing, each change with its log entry.
    let mut tx = db.begin().await?;
    for (key, value) in wanted {
        if current.get(key) == Some(&value) {
            continue;
        }
        settings::set_in(&mut tx, key, value.clone())
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(actor(&page), ActionKind::SettingUpdate)
                .details(json!({ "key": key, "from": current.get(key), "value": value })),
        )
        .await?;
    }
    tx.commit().await?;
    state.site.reload(db).await?;
    Ok(saved(jar, "/admin/settings"))
}

// ---- users ----------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct UserQuery {
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    /// A role id.
    #[serde(default)]
    role: String,
    /// Part of the email address.
    #[serde(default)]
    email: String,
    /// `yes` or `no`.
    #[serde(default)]
    banned: String,
    page: Option<i64>,
}

async fn user_list(page: Page, Query(query): Query<UserQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ManageUsers)?;
    let state = page.state();
    let site = state.site.get();
    let number = query.page.unwrap_or(1).clamp(1, 1000);
    let filter = users::UserFilter {
        name_prefix: query.name.trim(),
        status: UserStatus::parse(&query.status),
        role_id: query.role.trim().parse().ok(),
        email: query.email.trim(),
        banned: match query.banned.as_str() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        },
    };
    let found = users::list(
        state.db.primary(),
        &filter,
        (number - 1) * USERS_PAGE,
        USERS_PAGE + 1,
    )
    .await?;
    let more = found.len() > USERS_PAGE as usize;
    let my_rank = page.current.role.rank;
    let me = actor(&page);
    let assignable: Vec<Value> = site
        .roles()
        .iter()
        .filter(|r| may_assign(&page.current.role, r))
        .map(|r| context! { id => r.id, name => r.name })
        .collect();
    let ids: Vec<i64> = found.iter().map(|u| u.id).collect();
    let two_factor = moekura_db::two_factor::enabled_among(state.db.primary(), &ids).await?;
    let banned = moekura_db::bans::banned_among(state.db.primary(), &ids).await?;
    let rows: Vec<Value> = found
        .iter()
        .take(USERS_PAGE as usize)
        .map(|user| {
            let role = site.role(user.role_id);
            let editable = Some(user.id) != me && role.is_none_or(|r| r.rank < my_rank);
            context! {
                name => user.name,
                role_id => user.role_id,
                role => role.map(|r| r.name.clone()),
                role_assignable => role.is_some_and(|r| may_assign(&page.current.role, r)),
                status => user.status.as_str(),
                joined => user.created_at.date().to_string(),
                editable => editable,
                two_factor => two_factor.contains(&user.id),
                banned => banned.contains(&user.id),
                email => user.email,
            }
        })
        .collect();
    let url = |n: i64| {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", &query.name)
            .append_pair("status", &query.status)
            .append_pair("role", &query.role)
            .append_pair("email", &query.email)
            .append_pair("banned", &query.banned)
            .append_pair("page", &n.to_string())
            .finish();
        crate::templates::url_value(&format!("/admin/users?{q}"))
    };
    Ok(page.render(
        "admin_users.html",
        context! {
            users => rows,
            roles => assignable,
            all_roles => site.roles().iter().map(|r| context! { id => r.id.to_string(), name => r.name }).collect::<Vec<_>>(),
            query => context! {
                name => query.name,
                status => query.status,
                role => query.role,
                email => query.email,
                banned => query.banned,
            },
            previous_url => (number > 1).then(|| url(number - 1)),
            next_url => more.then(|| url(number + 1)),
        },
    ))
}

#[derive(Debug, Deserialize)]
struct UserForm {
    role: i32,
    status: String,
    /// Kept in the log.
    #[serde(default)]
    reason: String,
}

async fn update_user(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(form): Form<UserForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageUsers)?;
    let state = page.state();
    let db = state.db.primary();
    let site = state.site.get();
    let my_rank = page.current.role.rank;
    let user = users::by_name(db, &name).await?.ok_or(AppError::NotFound)?;
    let current_rank = site.role(user.role_id).map_or(0, |r| r.rank);
    if Some(user.id) == actor(&page) || current_rank >= my_rank {
        return Err(AppError::Forbidden);
    }
    // Keeping a user's role needs no say over it; changing it does.
    let role = site
        .role(form.role)
        .filter(|r| r.id == user.role_id || may_assign(&page.current.role, r))
        .ok_or_else(|| AppError::BadRequest("You can't give that role".into()))?;
    let status = UserStatus::parse(&form.status)
        .ok_or_else(|| AppError::BadRequest("Unknown status".into()))?;
    let reason = form.reason.trim();
    if reason.chars().count() > REASON_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "The reason may be at most {REASON_MAX_LEN} characters"
        )));
    }

    let mut tx = db.begin().await?;
    if role.id != user.role_id {
        users::set_role(&mut *tx, user.id, role.id).await?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(actor(&page), ActionKind::UserRole)
                .user(user.id)
                .reason(reason)
                .details(json!({
                    "from": site.role(user.role_id).map(|r| r.name.as_str()),
                    "role": role.name,
                })),
        )
        .await?;
    }
    if status != user.status {
        users::set_status(&mut *tx, user.id, status).await?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(actor(&page), ActionKind::UserStatus)
                .user(user.id)
                .reason(reason)
                .details(json!({ "from": user.status.as_str(), "status": status.as_str() })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(saved(jar, "/admin/users"))
}

/// Whether holders of `mine` may give users `role`: it ranks below
/// theirs, grants nothing they lack, and isn't the visitors' role.
fn may_assign(mine: &Role, role: &Role) -> bool {
    mine.outranks(role)
        && mine.permissions.contains_all(role.permissions)
        && role.system != Some(SystemRole::Anonymous)
}

// ---- roles ----------------------------------------------------------------

/// A role name as typed, checked.
fn role_name(text: &str) -> Result<&str, AppError> {
    let name = text.trim();
    if name.is_empty() || name.chars().count() > 32 {
        return Err(AppError::BadRequest(
            "A role name is 1 to 32 characters".into(),
        ));
    }
    Ok(name)
}

/// A rank as typed for a custom role: above the visitors' (0) and below
/// `mine`, so its holders stay within your reach.
fn role_rank(text: &str, mine: i16) -> Result<i16, AppError> {
    text.trim()
        .parse::<i16>()
        .ok()
        .filter(|rank| (1..mine).contains(rank))
        .ok_or_else(|| AppError::BadRequest(format!("A rank is a number from 1 to {}", mine - 1)))
}

fn name_taken(e: sqlx::Error) -> AppError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            AppError::BadRequest("Another role has that name".into())
        }
        _ => AppError::from(e),
    }
}

async fn role_list(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let site = page.state().site.get();
    let my_rank = page.current.role.rank;
    // Where a deleted role's users can go.
    let targets: Vec<(i32, &str)> = site
        .roles()
        .iter()
        .filter(|r| may_assign(&page.current.role, r))
        .map(|r| (r.id, r.name.as_str()))
        .collect();
    let rows: Vec<Value> = site
        .roles()
        .iter()
        .map(|role| {
            context! {
                id => role.id,
                name => role.name,
                rank => role.rank,
                editable => role.rank < my_rank,
                custom => role.system.is_none(),
                move_targets => targets
                    .iter()
                    .filter(|(id, _)| *id != role.id)
                    .map(|(id, name)| context! { id => id, name => name })
                    .collect::<Vec<_>>(),
                pending_limit => role.upload_limits.pending,
                daily_limit => role.upload_limits.daily,
                permissions => Permission::ALL.iter().map(|p| context! {
                    key => p.key(),
                    label => p.label(),
                    granted => role.can(*p),
                    // Only what you hold yourself can be handed on.
                    grantable => page.current.can(*p),
                }).collect::<Vec<_>>(),
            }
        })
        .collect();
    Ok(page.render(
        "admin_roles.html",
        context! {
            roles => rows,
            max_rank => my_rank - 1,
            templates => site.roles().iter().map(|r| context! { id => r.id, name => r.name }).collect::<Vec<_>>(),
        },
    ))
}

#[derive(Debug, Deserialize)]
struct NewRoleForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    rank: String,
    /// The id of a role whose permissions to start from, those you hold;
    /// empty for none.
    #[serde(default)]
    copy_from: String,
}

async fn create_role(
    page: Page,
    jar: CookieJar,
    Form(form): Form<NewRoleForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let db = state.db.primary();
    let site = state.site.get();
    let name = role_name(&form.name)?;
    let rank = role_rank(&form.rank, page.current.role.rank)?;
    let permissions = match form.copy_from.trim() {
        "" => Permissions::NONE,
        id => id
            .parse()
            .ok()
            .and_then(|id| site.role(id))
            .ok_or_else(|| AppError::BadRequest("There's no such role to copy".into()))?
            .permissions
            .and(page.current.role.permissions)
            .and(Permissions::of(&Permission::ALL)),
    };
    let mut tx = db.begin().await?;
    let id = roles::create(&mut tx, name, rank, permissions)
        .await
        .map_err(name_taken)?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor(&page), ActionKind::RoleCreate).details(json!({
            "role": name,
            "rank": rank,
            "permissions": permissions.iter().map(|p| p.key()).collect::<Vec<_>>(),
        })),
    )
    .await?;
    tx.commit().await?;
    state.site.reload(db).await?;
    Ok(saved(jar, &format!("/admin/roles#role-{id}")))
}

#[derive(Debug, Deserialize)]
struct DeleteRoleForm {
    /// The role its users move to.
    move_to: i32,
}

async fn delete_role(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<DeleteRoleForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let db = state.db.primary();
    let site = state.site.get();
    let role = site.role(id).ok_or(AppError::NotFound)?;
    if !page.current.role.outranks(role) {
        return Err(AppError::Forbidden);
    }
    if role.system.is_some() {
        return Err(AppError::BadRequest(
            "Built-in roles can't be deleted".into(),
        ));
    }
    let target = site
        .role(form.move_to)
        .filter(|r| r.id != id && may_assign(&page.current.role, r))
        .ok_or_else(|| AppError::BadRequest("You can't move its users to that role".into()))?;
    let mut tx = db.begin().await?;
    let moved = roles::delete(&mut tx, id, target.id)
        .await?
        .ok_or(AppError::NotFound)?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor(&page), ActionKind::RoleDelete).details(json!({
            "role": role.name,
            "moved_to": target.name,
            "users": moved,
        })),
    )
    .await?;
    tx.commit().await?;
    state.site.reload(db).await?;
    Ok(saved(jar, "/admin/roles"))
}

async fn update_role(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let db = state.db.primary();
    let site = state.site.get();
    let role = site.role(id).ok_or(AppError::NotFound)?;
    // Roles at or above your own (yours included) are out of reach, so no
    // one can take their own powers away by accident.
    if role.rank >= page.current.role.rank {
        return Err(AppError::Forbidden);
    }
    let field = |key: &str| form.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let name = role_name(field("name").unwrap_or(""))?;
    // Built-in roles keep their ranks; custom ones may move below yours.
    let rank = match field("rank") {
        Some(text) if role.system.is_none() => Some(role_rank(text, page.current.role.rank)?),
        _ => None,
    };
    // Permissions the editor holds are set from the form; the rest (ones
    // they lack, and bits this version doesn't know) stay as they were.
    let mine = page
        .current
        .role
        .permissions
        .and(Permissions::of(&Permission::ALL));
    let ticked: Vec<Permission> = Permission::ALL
        .into_iter()
        .filter(|p| form.iter().any(|(k, _)| k == p.key()))
        .collect();
    let permissions = role
        .permissions
        .without(mine)
        .with(Permissions::of(&ticked).and(mine));
    let granted: Vec<Permission> = permissions.iter().collect();
    let limit = |key: &str| -> Result<Option<i32>, AppError> {
        match form.iter().find(|(k, _)| k == key).map(|(_, v)| v.trim()) {
            None | Some("") => Ok(None),
            Some(value) => value
                .parse::<i32>()
                .ok()
                .filter(|n| *n >= 0)
                .map(Some)
                .ok_or_else(|| {
                    AppError::BadRequest("An upload limit is a number, or empty for none".into())
                }),
        }
    };
    let limits = UploadLimits {
        pending: limit("pending_upload_limit")?,
        daily: limit("daily_upload_limit")?,
    };
    let mut tx = db.begin().await?;
    roles::update(&mut tx, id, name, permissions, limits)
        .await
        .map_err(name_taken)?;
    if let Some(rank) = rank {
        roles::set_rank(&mut tx, id, rank).await?;
    }
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor(&page), ActionKind::RoleUpdate).details(json!({
            "role": name,
            "permissions": granted.iter().map(|p| p.key()).collect::<Vec<_>>(),
            "rank": rank.unwrap_or(role.rank),
            "pending_upload_limit": limits.pending,
            "daily_upload_limit": limits.daily,
        })),
    )
    .await?;
    tx.commit().await?;
    state.site.reload(db).await?;
    Ok(saved(jar, "/admin/roles"))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::{Permission, SystemRole};
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(
            test_state(pool).await,
            super::routes().merge(crate::posts::routes()),
        )
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn overview_and_dead_jobs(pool: PgPool) {
        let app = app(&pool).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let job: i64 = sqlx::query_scalar(
            "INSERT INTO jobs (kind, status, attempts, last_error) VALUES ('media.process', 'dead', 5, 'vips crashed')
             RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let page = app.get("/admin", Some(&admin)).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("vips crashed"), "{}", page.body);
        let response = app
            .post(&format!("/admin/jobs/{job}/retry"), Some(&admin), &[])
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "queued");
        let again = app
            .post(&format!("/admin/jobs/{job}/discard"), Some(&admin), &[])
            .await;
        assert_eq!(again.status, StatusCode::BAD_REQUEST, "only dead jobs");
        let member = session_for(&pool, "alice", SystemRole::Member).await;
        assert_eq!(
            app.get("/admin", Some(&member)).await.status,
            StatusCode::FORBIDDEN
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn site_settings(pool: PgPool) {
        let app = app(&pool).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        assert_eq!(
            app.get("/admin/settings", Some(&moderator)).await.status,
            StatusCode::FORBIDDEN
        );
        let response = app
            .post_form(
                "/admin/settings",
                Some(&admin),
                &[],
                "site_name=Tiny+Booru&registration_mode=invite&upload_approval=on&default_blacklist=rating%3Ae\
                 &auto_promotion=on&promotion_uploads=20&promotion_edits=5&promotion_account_days=14\
                 &promotion_max_recent_deletions=1",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let stored = moekura_db::settings::load(&pool).await.unwrap();
        assert_eq!(stored.site_name, "Tiny Booru");
        assert!(stored.upload_approval);
        assert!(stored.auto_promotion);
        assert_eq!(
            stored.promotion_rules,
            moekura_core::promotion::Rules {
                uploads: 20,
                edits: 5,
                account_days: 14,
                max_recent_deletions: 1
            }
        );
        let bad = app
            .post_form(
                "/admin/settings",
                Some(&admin),
                &[],
                "site_name=Tiny+Booru&registration_mode=invite&promotion_uploads=lots",
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        // The page title follows at once on this node.
        assert!(
            app.get("/", None)
                .await
                .body
                .contains("<title>Tiny Booru</title>")
        );

        let bad = app
            .post_form(
                "/admin/settings",
                Some(&admin),
                &[],
                "site_name=&registration_mode=open&default_blacklist=",
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            moekura_db::settings::load(&pool).await.unwrap().site_name,
            "Tiny Booru",
            "nothing changed"
        );
        let logged: i64 =
            sqlx::query_scalar("SELECT count(*) FROM mod_actions WHERE action = 'setting.update'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(logged, 6);

        // The tagger: thresholds by category, a blank one falling back to
        // general's.
        let response = app
            .post_form(
                "/admin/settings",
                Some(&admin),
                &[],
                "site_name=Tiny+Booru&registration_mode=invite&promotion_uploads=20\
                 &promotion_edits=5&promotion_account_days=14&promotion_max_recent_deletions=1\
                 &tagger_threshold_general=40&tagger_threshold_character=\
                 &tagger_threshold_meta=70&tagger_auto_apply=on&tagger_auto_threshold=92",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let tagger = moekura_db::settings::load(&pool).await.unwrap().tagger;
        assert_eq!(
            tagger.thresholds,
            [("general".to_owned(), 40), ("meta".to_owned(), 70)].into()
        );
        assert!(tagger.auto_apply && !tagger.auto_rating);
        assert_eq!(tagger.auto_threshold, 92);
        let page = app.get("/admin/settings", Some(&admin)).await;
        assert!(
            page.body.contains(
                "name=\"tagger_threshold_meta\" type=\"number\" min=\"1\" max=\"100\" value=\"70\""
            ),
            "{}",
            page.body
        );
        let bad = app
            .post_form(
                "/admin/settings",
                Some(&admin),
                &[],
                "site_name=Tiny+Booru&registration_mode=invite&promotion_uploads=20\
                 &promotion_edits=5&promotion_account_days=14&promotion_max_recent_deletions=1\
                 &tagger_threshold_general=0",
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(bad.body.contains("from 1 to 100"), "{}", bad.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn users_and_roles(pool: PgPool) {
        let app = app(&pool).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        session_for(&pool, "alice", SystemRole::Member).await;
        let site = moekura_db::site_cache::SiteCache::load(&pool)
            .await
            .unwrap()
            .get();
        let role_id = |r: SystemRole| site.system_role(r).unwrap().id;

        assert_eq!(
            app.get("/admin/users", Some(&moderator)).await.status,
            StatusCode::FORBIDDEN
        );
        let list = app.get("/admin/users?name=al", Some(&admin)).await.body;
        assert!(
            list.contains("<td><a href=\"/users/alice\">alice</a></td>"),
            "{list}"
        );
        assert!(
            !list.contains("<td><a href=\"/users/root\">"),
            "filtered by name"
        );
        let staff = app
            .get(
                &format!("/admin/users?role={}", role_id(SystemRole::Moderator)),
                Some(&admin),
            )
            .await
            .body;
        assert!(staff.contains("href=\"/users/mod\""), "{staff}");
        assert!(!staff.contains("href=\"/users/alice\""), "filtered by role");
        let alice_id = moekura_db::users::by_name(&pool, "alice")
            .await
            .unwrap()
            .unwrap()
            .id;
        let mut tx = pool.begin().await.unwrap();
        moekura_db::bans::ban(&mut tx, alice_id, "spam", None, None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let banned = app.get("/admin/users?banned=yes", Some(&admin)).await.body;
        assert!(banned.contains("href=\"/users/alice\""), "{banned}");
        assert!(banned.contains("banned"), "{banned}");
        assert!(!banned.contains("href=\"/users/mod\""));
        let not_banned = app.get("/admin/users?banned=no", Some(&admin)).await.body;
        assert!(!not_banned.contains("href=\"/users/alice\""));
        moekura_db::bans::lift(&pool, alice_id, None).await.unwrap();
        sqlx::query("UPDATE users SET email = 'Alice@Example.org' WHERE id = $1")
            .bind(alice_id)
            .execute(&pool)
            .await
            .unwrap();
        let by_email = app
            .get("/admin/users?email=example.ORG", Some(&admin))
            .await
            .body;
        assert!(by_email.contains("href=\"/users/alice\""), "{by_email}");
        assert!(!by_email.contains("href=\"/users/mod\""));
        // Nobody hands out their own rank or edits themselves.
        let form = format!("role={}&status=active", role_id(SystemRole::Admin));
        assert_eq!(
            app.post_form("/admin/users/alice", Some(&admin), &[], &form)
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
        let form = format!("role={}&status=active", role_id(SystemRole::Member));
        assert_eq!(
            app.post_form("/admin/users/root", Some(&admin), &[], &form)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let form = format!(
            "role={}&status=deactivated&reason=asked+to+leave",
            role_id(SystemRole::Contributor)
        );
        let response = app
            .post_form("/admin/users/alice", Some(&admin), &[], &form)
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let alice = moekura_db::users::by_name(&pool, "alice")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(alice.role_id, role_id(SystemRole::Contributor));
        assert_eq!(alice.status, moekura_db::users::UserStatus::Deactivated);
        let reasons: Vec<String> =
            sqlx::query_scalar("SELECT reason FROM mod_actions WHERE user_id = $1")
                .bind(alice.id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(reasons, ["asked to leave", "asked to leave"]);
        let details: Vec<serde_json::Value> =
            sqlx::query_scalar("SELECT details FROM mod_actions WHERE user_id = $1 ORDER BY id")
                .bind(alice.id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            details,
            [
                serde_json::json!({ "from": "Member", "role": "Contributor" }),
                serde_json::json!({ "from": "active", "status": "deactivated" }),
            ]
        );

        // Roles: admins edit lower roles, not their own.
        let roles = app.get("/admin/roles", Some(&admin)).await.body;
        assert!(roles.contains("Upload without approval"), "{roles}");
        let anonymous = role_id(SystemRole::Anonymous);
        let response = app
            .post_form(
                &format!("/admin/roles/{anonymous}"),
                Some(&admin),
                &[],
                "name=Visitor",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        // Without view_posts, visitors are sent to log in: a private site.
        let home = app.get("/", None).await;
        assert_eq!(home.status, StatusCode::SEE_OTHER);
        // Upload limits: numbers, or empty for none.
        let member = role_id(SystemRole::Member);
        let saved = app
            .post_form(
                &format!("/admin/roles/{member}"),
                Some(&admin),
                &[],
                "name=Member&upload=on&pending_upload_limit=3&daily_upload_limit=",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        let limits: (Option<i32>, Option<i32>) = sqlx::query_as(
            "SELECT pending_upload_limit, daily_upload_limit FROM roles WHERE id = $1",
        )
        .bind(member)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(limits, (Some(3), None));
        let bad = app
            .post_form(
                &format!("/admin/roles/{member}"),
                Some(&admin),
                &[],
                "name=Member&daily_upload_limit=-1",
            )
            .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert!(
            app.get("/admin/roles", Some(&admin))
                .await
                .body
                .contains("name=\"pending_upload_limit\" type=\"number\" min=\"0\" value=\"3\"")
        );
        let own = role_id(SystemRole::Admin);
        assert_eq!(
            app.post_form(
                &format!("/admin/roles/{own}"),
                Some(&admin),
                &[],
                "name=Admin"
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn staff_hand_on_only_what_they_hold(pool: PgPool) {
        // Moderators who may manage settings and users, and contributors
        // who may purge, which moderators may not.
        sqlx::query(
            "UPDATE roles SET permissions = permissions | (1::bigint << 14) | (1::bigint << 15)
             WHERE system_key = 'moderator'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE roles SET permissions = permissions | (1::bigint << 10)
             WHERE system_key = 'contributor'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let app = app(&pool).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        session_for(&pool, "alice", SystemRole::Member).await;
        let site = moekura_db::site_cache::SiteCache::load(&pool)
            .await
            .unwrap()
            .get();
        let role = |r: SystemRole| site.system_role(r).unwrap().clone();
        let permissions = |id: i32| {
            let pool = pool.clone();
            async move {
                let bits: i64 = sqlx::query_scalar("SELECT permissions FROM roles WHERE id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                moekura_core::permissions::Permissions::from_db(bits)
            }
        };

        // Ticking purge_posts for janitors does nothing.
        let janitor = role(SystemRole::Janitor);
        let mut form = String::from("name=Janitor");
        for p in janitor.permissions.iter() {
            form.push_str(&format!("&{}=on", p.key()));
        }
        form.push_str("&purge_posts=on");
        let saved = app
            .post_form(
                &format!("/admin/roles/{}", janitor.id),
                Some(&moderator),
                &[],
                &form,
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        assert_eq!(permissions(janitor.id).await, janitor.permissions);
        // Unticked, contributors keep it: it isn't the moderator's to take.
        let contributor = role(SystemRole::Contributor);
        let saved = app
            .post_form(
                &format!("/admin/roles/{}", contributor.id),
                Some(&moderator),
                &[],
                "name=Contributor&view_posts=on",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        assert_eq!(
            permissions(contributor.id).await.iter().collect::<Vec<_>>(),
            [Permission::ViewPosts, Permission::PurgePosts]
        );
        let roles = app.get("/admin/roles", Some(&moderator)).await.body;
        assert!(roles.contains("name=\"purge_posts\" disabled"), "{roles}");

        // Nor can they make someone a contributor now.
        let form = format!("role={}&status=active", contributor.id);
        assert_eq!(
            app.post_form("/admin/users/alice", Some(&moderator), &[], &form)
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
        let list = app.get("/admin/users", Some(&moderator)).await.body;
        let picker = list
            .split("aria-label=\"Role of alice\"")
            .nth(1)
            .and_then(|rest| rest.split("</select>").next())
            .expect("a role picker for alice");
        assert!(
            !picker.contains(&format!("<option value=\"{}\">", contributor.id)),
            "{picker}"
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn custom_roles(pool: PgPool) {
        let app = app(&pool).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        session_for(&pool, "alice", SystemRole::Member).await;
        let site = moekura_db::site_cache::SiteCache::load(&pool)
            .await
            .unwrap()
            .get();
        let member = site.system_role(SystemRole::Member).unwrap().clone();
        let role = |name: &'static str| {
            let pool = pool.clone();
            async move { moekura_db::roles::by_name(&pool, name).await.unwrap() }
        };

        // Out of reach: rank 50 is the admin's own.
        let too_high = app
            .post_form(
                "/admin/roles/new",
                Some(&admin),
                &[],
                "name=Trusted&rank=50&copy_from=",
            )
            .await;
        assert_eq!(too_high.status, StatusCode::BAD_REQUEST);
        let created = app
            .post_form(
                "/admin/roles/new",
                Some(&admin),
                &[],
                &format!("name=Trusted&rank=15&copy_from={}", member.id),
            )
            .await;
        assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
        let trusted = role("Trusted").await.unwrap();
        assert_eq!(trusted.rank, 15);
        assert_eq!(trusted.permissions, member.permissions);
        let taken = app
            .post_form(
                "/admin/roles/new",
                Some(&admin),
                &[],
                "name=trusted&rank=15&copy_from=",
            )
            .await;
        assert_eq!(taken.status, StatusCode::BAD_REQUEST);

        // Re-ranked with the rest of the form; built-in ranks stay.
        let saved = app
            .post_form(
                &format!("/admin/roles/{}", trusted.id),
                Some(&admin),
                &[],
                "name=Trusted&rank=25&upload=on",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        assert_eq!(role("Trusted").await.unwrap().rank, 25);
        app.post_form(
            &format!("/admin/roles/{}", member.id),
            Some(&admin),
            &[],
            "name=Member&rank=5&upload=on",
        )
        .await;
        assert_eq!(role("Member").await.unwrap().rank, 10);

        // Deleting moves its users; built-in roles can't go.
        let form = format!("role={}&status=active", trusted.id);
        app.post_form("/admin/users/alice", Some(&admin), &[], &form)
            .await;
        let refused = app
            .post_form(
                &format!("/admin/roles/{}/delete", member.id),
                Some(&admin),
                &[],
                &format!("move_to={}", trusted.id),
            )
            .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        let deleted = app
            .post_form(
                &format!("/admin/roles/{}/delete", trusted.id),
                Some(&admin),
                &[],
                &format!("move_to={}", member.id),
            )
            .await;
        assert_eq!(deleted.status, StatusCode::SEE_OTHER, "{}", deleted.body);
        assert!(role("Trusted").await.is_none());
        let alice = moekura_db::users::by_name(&pool, "alice")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(alice.role_id, member.id);
        let logged: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM mod_actions WHERE action LIKE 'role.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            logged,
            ["role.create", "role.update", "role.update", "role.delete"]
        );
    }
}
