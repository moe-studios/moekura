//! Users and the requester's own account.

use axum::Json;
use axum::extract::{Path, State};
use moekura_core::permissions::Permission;
use moekura_core::user_settings::UserSettings;
use moekura_db::users::{self, UserStatus};
use moekura_db::{favorites, posts};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::{AppError, ErrorBody};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiUser {
    pub name: String,
    /// The role's display name, which admins may change.
    pub role: Option<String>,
    /// The built-in role's key, which never changes (`member`,
    /// `contributor`, `janitor`, `moderator` or `admin`); `null` for a
    /// role the site made.
    pub role_key: Option<String>,
    /// `active`, `pending` (awaiting approval) or `deactivated`.
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub upload_count: i64,
    pub favorite_count: i64,
    /// What they wrote about themselves, in markup; empty for nothing.
    pub bio: String,
    /// Their profile picture: square, at most 400 pixels a side.
    pub avatar_url: Option<String>,
    /// Their banner: 3:1, at most 1500×500 pixels.
    pub banner_url: Option<String>,
}

/// Get a user.
///
/// Needs `view_posts`. Only users who can manage users see inactive
/// accounts.
#[utoipa::path(
    get,
    path = "/users/{name}",
    operation_id = "get_user",
    tag = "users",
    params(("name" = String, Path, description = "Case-insensitive")),
    responses((status = 200, body = ApiUser), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(name): Path<String>,
) -> Result<Json<ApiUser>, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let user = users::by_name(db, &name)
        .await?
        .filter(|u| u.status == UserStatus::Active || current.can(Permission::ManageUsers))
        .ok_or(AppError::NotFound)?;
    let site = state.site.get();
    let role = site.role(user.role_id);
    let role_key = role.and_then(|r| r.system).map(|s| s.key().to_owned());
    let role = role.map(|r| r.name.clone());
    let profile = users::profile(db, user.id).await?;
    let url = |key: Option<String>| {
        key.as_deref()
            .and_then(moekura_storage::Key::parse)
            .map(|k| state.file_url(&k))
    };
    Ok(Json(ApiUser {
        avatar_url: url(profile.avatar_key),
        banner_url: url(profile.banner_key),
        bio: profile.bio,
        upload_count: posts::count_by_uploader(db, user.id).await?,
        favorite_count: favorites::count_by_user(db, user.id).await?,
        name: user.name,
        role,
        role_key,
        status: user.status.as_str().to_owned(),
        created_at: user.created_at,
    }))
}

/// The account making the request.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiMe {
    pub name: String,
    /// The role's display name, which admins may change.
    pub role: String,
    /// The built-in role's key, which never changes; `null` for a role
    /// the site made.
    pub role_key: Option<String>,
    /// What this account may do now (`upload`, `edit_posts`, …). While
    /// banned, only what visitors may.
    pub permissions: Vec<String>,
    pub settings: ApiSettings,
    /// Set while banned.
    pub ban: Option<ApiActiveBan>,
    pub upload_limits: ApiUploadAllowance,
}

/// How many more posts the account may upload now.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiUploadAllowance {
    /// Why uploading is refused now, if it is.
    pub refused: Option<String>,
    /// Uploads left before the approval queue's limit; `null` if none
    /// applies.
    pub pending_left: Option<i64>,
    /// Uploads left today; `null` if there's no daily limit.
    pub today_left: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiSettings {
    /// Posts per page on the site; `null` for the site's default.
    pub per_page: Option<u32>,
    /// `system`, `light` or `dark`.
    pub mode: String,
    /// The colour theme shown: the account's choice, or the site's default.
    pub theme: String,
    /// The blacklist in effect: the account's own, or the site's default.
    pub blacklist: String,
    /// Only general-rated posts are shown, here too.
    pub safe_mode: bool,
    /// An IANA name such as `Europe/Berlin`; `null` for UTC.
    pub time_zone: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiActiveBan {
    pub reason: String,
    /// `null` for a permanent ban.
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

/// Get your account.
///
/// Needs authentication.
#[utoipa::path(
    get,
    path = "/me",
    operation_id = "get_me",
    tag = "users",
    responses((status = 200, body = ApiMe), (status = 401, body = ErrorBody)),
)]
pub(crate) async fn me(
    State(state): State<AppState>,
    current: CurrentUser,
) -> Result<Json<ApiMe>, AppError> {
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let settings = UserSettings::from_json(&user.settings);
    Ok(Json(ApiMe {
        name: user.name.clone(),
        role: current.role.name.clone(),
        role_key: current.role.system.map(|s| s.key().to_owned()),
        permissions: current
            .role
            .permissions
            .iter()
            .map(|p| p.key().to_owned())
            .collect(),
        settings: ApiSettings {
            per_page: settings.per_page,
            mode: settings.mode.as_str().to_owned(),
            theme: crate::themes::resolve(
                &state.assets,
                settings.theme.as_deref(),
                &state.site.get().settings.default_theme,
            )
            .to_owned(),
            blacklist: crate::blacklist::text_for(&state, &current),
            safe_mode: settings.safe_mode,
            time_zone: settings.time_zone.clone(),
        },
        upload_limits: {
            let allowance = crate::upload::allowance(&state, &current).await?;
            ApiUploadAllowance {
                refused: allowance.refusal,
                pending_left: allowance.pending_left,
                today_left: allowance.today_left,
            }
        },
        ban: current.ban.as_ref().map(|ban| ApiActiveBan {
            reason: ban.reason.clone(),
            expires_at: ban.expires_at,
        }),
    }))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::json;
    use sqlx::PgPool;

    use crate::api::test_support::{app, json, upload};
    use crate::test_support::{fixture, session_for};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn shows_users_and_the_requester(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        upload(&app, &alice, &fixture::png(20, 20), "cat").await;

        let user = json(&app.get("/api/v1/users/ALICE", None).await.body);
        assert_eq!(user["name"], json!("alice"));
        assert_eq!(user["role"], json!("Member"));
        assert_eq!(user["role_key"], json!("member"));
        assert_eq!(user["upload_count"], json!(1));

        let me = json(&app.get("/api/v1/me", Some(&alice)).await.body);
        assert_eq!(me["name"], json!("alice"));
        assert_eq!(me["role_key"], json!("member"));
        assert!(
            me["permissions"]
                .as_array()
                .unwrap()
                .contains(&json!("upload")),
            "{me}"
        );
        assert_eq!(me["ban"], json!(null));
        assert_eq!(
            app.get("/api/v1/me", None).await.status,
            StatusCode::UNAUTHORIZED
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn site_made_roles_have_no_key(pool: PgPool) {
        session_for(&pool, "bob", SystemRole::Member).await;
        let mut conn = pool.acquire().await.unwrap();
        let role = moekura_db::roles::create(
            &mut conn,
            "Curator",
            150,
            moekura_core::permissions::Permissions::NONE,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE users SET role_id = $1 WHERE name = 'bob'")
            .bind(role)
            .execute(&pool)
            .await
            .unwrap();
        // Built after the role, so the site's cache has it.
        let app = app(&pool).await;
        let user = json(&app.get("/api/v1/users/bob", None).await.body);
        assert_eq!(
            (&user["role"], &user["role_key"]),
            (&json!("Curator"), &json!(null))
        );
    }
}
