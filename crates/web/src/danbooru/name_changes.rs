//! `/user_name_change_requests.json` and
//! `/user_name_change_requests/{id}.json`: users' name changes (which,
//! unlike Danbooru's requests, take effect at once). Inactive users'
//! changes only for staff who manage users, as their profiles are.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::name_changes::{self, Filter, NameChange};
use moekura_db::users::{self, UserStatus};
use serde::{Deserialize, Serialize};

use super::tags::window;
use super::{ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/user_name_change_requests", get(index))
        .route("/user_name_change_requests/{id}", get(show))
}

#[derive(Debug, Serialize)]
struct DanbooruNameChange {
    id: i64,
    user_id: i64,
    original_name: String,
    desired_name: String,
    created_at: String,
    updated_at: String,
}

impl From<NameChange> for DanbooruNameChange {
    fn from(c: NameChange) -> Self {
        let at = timestamp(c.created_at);
        Self {
            id: c.id,
            user_id: c.user_id,
            original_name: c.old_name,
            desired_name: c.new_name,
            updated_at: at.clone(),
            created_at: at,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct Params {
    #[serde(rename = "search[user_id]", default)]
    user_id: String,
    #[serde(rename = "search[original_name]", default)]
    original_name: String,
    #[serde(rename = "search[desired_name]", default)]
    desired_name: String,
    #[serde(flatten)]
    list: ListParams,
}

fn named(text: &str) -> Option<&str> {
    Some(text.trim()).filter(|t| !t.is_empty())
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<Params>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let (offset, limit) = window(&params.list, 1000)?;
    let filter = Filter {
        user_id: params.user_id.trim().parse().ok(),
        old_name: named(&params.original_name),
        new_name: named(&params.desired_name),
        active_only: !current.can(Permission::ManageUsers),
    };
    let list: Vec<DanbooruNameChange> =
        name_changes::list(state.reader(&current), &filter, offset, limit)
            .await?
            .into_iter()
            .map(DanbooruNameChange::from)
            .collect();
    json(list, &params.list.only)
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let db = state.reader(&current);
    let change = name_changes::by_id(db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !current.can(Permission::ManageUsers) {
        users::by_id(db, change.user_id)
            .await?
            .filter(|u| u.status == UserStatus::Active)
            .ok_or(AppError::NotFound)?;
    }
    json(DanbooruNameChange::from(change), "")
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::danbooru::test_support::app;
    use crate::test_support::{TestApp, session_for};

    async fn original_names(app: &TestApp, session: Option<&str>) -> Vec<String> {
        let response = app.get("/user_name_change_requests.json", session).await;
        let list: Value = serde_json::from_str(&response.body).unwrap();
        list.as_array()
            .unwrap()
            .iter()
            .map(|c| c["original_name"].as_str().unwrap().to_owned())
            .collect()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn inactive_users_renames_only_for_staff(pool: PgPool) {
        let app = app(&pool).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let mut changes = Vec::new();
        for (name, status) in [("alice", "active"), ("carol", "deactivated")] {
            session_for(&pool, name, SystemRole::Member).await;
            let id: i64 =
                sqlx::query_scalar("UPDATE users SET status = $2 WHERE name = $1 RETURNING id")
                    .bind(name)
                    .bind(status)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            moekura_db::name_changes::rename(&pool, id, &format!("{name}_2"), Some(id))
                .await
                .unwrap();
            let change: i64 =
                sqlx::query_scalar("SELECT id FROM user_name_changes WHERE user_id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            changes.push(change);
        }
        assert_eq!(original_names(&app, None).await, ["alice"]);
        assert_eq!(original_names(&app, Some(&admin)).await, ["carol", "alice"]);
        let one = |id: i64| format!("/user_name_change_requests/{id}.json");
        assert_eq!(app.get(&one(changes[0]), None).await.status, StatusCode::OK);
        assert_eq!(
            app.get(&one(changes[1]), None).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.get(&one(changes[1]), Some(&admin)).await.status,
            StatusCode::OK
        );
    }
}
