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
    pub role: Option<String>,
    /// `active`, `pending` (awaiting approval) or `deactivated`.
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub uploads: i64,
    pub favorites: i64,
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
    let role = state.site.get().role(user.role_id).map(|r| r.name.clone());
    Ok(Json(ApiUser {
        uploads: posts::count_by_uploader(db, user.id).await?,
        favorites: favorites::count_by_user(db, user.id).await?,
        name: user.name,
        role,
        status: user.status.as_str().to_owned(),
        created_at: user.created_at,
    }))
}

/// The account making the request.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiMe {
    pub name: String,
    pub role: String,
    /// What this account may do now (`upload`, `edit_posts`, …). While
    /// banned, only what visitors may.
    pub permissions: Vec<String>,
    pub settings: ApiSettings,
    /// Set while banned.
    pub ban: Option<ApiActiveBan>,
    pub uploads: ApiUploadAllowance,
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
    pub theme: String,
    /// The blacklist in effect: the account's own, or the site's default.
    pub blacklist: String,
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
        permissions: current
            .role
            .permissions
            .iter()
            .map(|p| p.key().to_owned())
            .collect(),
        settings: ApiSettings {
            per_page: settings.per_page,
            theme: settings.theme.as_str().to_owned(),
            blacklist: crate::blacklist::text_for(&state, &current),
        },
        uploads: {
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
        assert_eq!(user["uploads"], json!(1));

        let me = json(&app.get("/api/v1/me", Some(&alice)).await.body);
        assert_eq!(me["name"], json!("alice"));
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
}
