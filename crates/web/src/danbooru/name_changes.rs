//! `/user_name_change_requests.json` and
//! `/user_name_change_requests/{id}.json`: users' name changes (which,
//! unlike Danbooru's requests, take effect at once).

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::name_changes::{self, Filter, NameChange};
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
    let change = name_changes::by_id(state.reader(&current), id)
        .await?
        .ok_or(AppError::NotFound)?;
    json(DanbooruNameChange::from(change), "")
}
