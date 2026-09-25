//! Admin pages: site settings, users and roles.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::{Permission, Permissions};
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
        .route("/admin/roles/{id}", post(update_role))
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
    let db = page.state().db.primary();
    let (done, kind) = match action.as_str() {
        "retry" => (jobs::retry(db, id).await?, ActionKind::JobRetry),
        "discard" => (jobs::discard(db, id).await?, ActionKind::JobDiscard),
        _ => return Err(AppError::NotFound),
    };
    if !done {
        return Err(AppError::BadRequest("That job isn't dead any more".into()));
    }
    mod_actions::record(
        db,
        NewAction::new(actor(&page), kind).details(json!({ "job": id })),
    )
    .await?;
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
    error: Option<String>,
    status: StatusCode,
) -> Response {
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
                default_blacklist => current.default_blacklist,
            },
            mail_enabled => page.state().config.mail.is_enabled(),
            modes => ["open", "invite", "approval", "closed"],
            error => error,
        },
    )
}

async fn settings_form(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let current = page.state().site.get().settings.clone();
    Ok(render_settings(&page, &current, None, StatusCode::OK))
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
    #[serde(default)]
    default_blacklist: String,
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
        (
            "default_blacklist",
            json!(form.default_blacklist.replace("\r\n", "\n").trim()),
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
                    Some(error.to_string()),
                    StatusCode::UNPROCESSABLE_ENTITY,
                ));
            }
        }
    }
    for (key, value) in wanted {
        if current.get(key) == Some(&value) {
            continue;
        }
        settings::set(db, key, value.clone())
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        mod_actions::record(
            db,
            NewAction::new(actor(&page), ActionKind::SettingUpdate)
                .details(json!({ "key": key, "value": value })),
        )
        .await?;
    }
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
    page: Option<i64>,
}

async fn user_list(page: Page, Query(query): Query<UserQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ManageUsers)?;
    let state = page.state();
    let site = state.site.get();
    let number = query.page.unwrap_or(1).clamp(1, 1000);
    let found = users::list(
        state.db.primary(),
        query.name.trim(),
        UserStatus::parse(&query.status),
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
        .filter(|r| {
            r.rank < my_rank && r.system != Some(moekura_core::permissions::SystemRole::Anonymous)
        })
        .map(|r| context! { id => r.id, name => r.name })
        .collect();
    let ids: Vec<i64> = found.iter().map(|u| u.id).collect();
    let two_factor = moekura_db::two_factor::enabled_among(state.db.primary(), &ids).await?;
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
                status => user.status.as_str(),
                joined => user.created_at.date().to_string(),
                editable => editable,
                two_factor => two_factor.contains(&user.id),
            }
        })
        .collect();
    let url = |n: i64| {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", &query.name)
            .append_pair("status", &query.status)
            .append_pair("page", &n.to_string())
            .finish();
        crate::templates::url_value(&format!("/admin/users?{q}"))
    };
    Ok(page.render(
        "admin_users.html",
        context! {
            users => rows,
            roles => assignable,
            query => context! { name => query.name, status => query.status },
            previous_url => (number > 1).then(|| url(number - 1)),
            next_url => more.then(|| url(number + 1)),
        },
    ))
}

#[derive(Debug, Deserialize)]
struct UserForm {
    role: i32,
    status: String,
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
    let role = site
        .role(form.role)
        .filter(|r| {
            r.rank < my_rank && r.system != Some(moekura_core::permissions::SystemRole::Anonymous)
        })
        .ok_or_else(|| AppError::BadRequest("You can't give that role".into()))?;
    let status = UserStatus::parse(&form.status)
        .ok_or_else(|| AppError::BadRequest("Unknown status".into()))?;

    let mut tx = db.begin().await?;
    if role.id != user.role_id {
        users::set_role(&mut *tx, user.id, role.id).await?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(actor(&page), ActionKind::UserRole)
                .user(user.id)
                .details(json!({ "role": role.name })),
        )
        .await?;
    }
    if status != user.status {
        users::set_status(&mut *tx, user.id, status).await?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(actor(&page), ActionKind::UserStatus)
                .user(user.id)
                .details(json!({ "status": status.as_str() })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(saved(jar, "/admin/users"))
}

// ---- roles ----------------------------------------------------------------

async fn role_list(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let site = page.state().site.get();
    let my_rank = page.current.role.rank;
    let rows: Vec<Value> = site
        .roles()
        .iter()
        .map(|role| {
            context! {
                id => role.id,
                name => role.name,
                rank => role.rank,
                editable => role.rank < my_rank,
                pending_limit => role.upload_limits.pending,
                daily_limit => role.upload_limits.daily,
                permissions => Permission::ALL.iter().map(|p| context! {
                    key => p.key(),
                    label => p.label(),
                    granted => role.can(*p),
                }).collect::<Vec<_>>(),
            }
        })
        .collect();
    Ok(page.render("admin_roles.html", context! { roles => rows }))
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
    let name = form
        .iter()
        .find(|(k, _)| k == "name")
        .map_or("", |(_, v)| v.trim());
    if name.is_empty() || name.chars().count() > 32 {
        return Err(AppError::BadRequest(
            "A role name is 1 to 32 characters".into(),
        ));
    }
    let granted: Vec<Permission> = Permission::ALL
        .into_iter()
        .filter(|p| form.iter().any(|(k, _)| k == p.key()))
        .collect();
    // Bits of permissions this version doesn't know stay as they were.
    let known = Permissions::of(&Permission::ALL);
    let unknown = role.permissions.bits() & !known.bits();
    let permissions = Permissions::from_bits(unknown).with(Permissions::of(&granted));
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
    roles::update(db, id, name, permissions, limits)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                AppError::BadRequest("Another role has that name".into())
            }
            _ => AppError::from(e),
        })?;
    mod_actions::record(
        db,
        NewAction::new(actor(&page), ActionKind::RoleUpdate).details(json!({
            "role": name,
            "permissions": granted.iter().map(|p| p.key()).collect::<Vec<_>>(),
            "pending_upload_limit": limits.pending,
            "daily_upload_limit": limits.daily,
        })),
    )
    .await?;
    state.site.reload(db).await?;
    Ok(saved(jar, "/admin/roles"))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
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
                "site_name=Tiny+Booru&registration_mode=invite&upload_approval=on&default_blacklist=rating%3Ae",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let stored = moekura_db::settings::load(&pool).await.unwrap();
        assert_eq!(stored.site_name, "Tiny Booru");
        assert!(stored.upload_approval);
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
        assert_eq!(logged, 4);
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
            "role={}&status=deactivated",
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
}
