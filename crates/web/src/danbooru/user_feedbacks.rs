//! `/user_feedbacks.json` and `/user_feedbacks/{id}.json`.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::user_feedbacks::{self, CATEGORIES, Feedback, Filter};
use serde::{Deserialize, Serialize};

use super::tags::window;
use super::{ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/user_feedbacks", get(index))
        .route("/user_feedbacks/{id}", get(show))
}

#[derive(Debug, Serialize)]
struct DanbooruFeedback {
    id: i64,
    user_id: i64,
    creator_id: Option<i64>,
    category: String,
    body: String,
    is_deleted: bool,
    created_at: String,
    updated_at: String,
}

impl From<Feedback> for DanbooruFeedback {
    fn from(f: Feedback) -> Self {
        Self {
            id: f.id,
            user_id: f.user_id,
            creator_id: f.creator_id,
            category: f.category,
            body: f.body,
            is_deleted: f.is_deleted,
            created_at: timestamp(f.created_at),
            updated_at: timestamp(f.updated_at),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct Params {
    #[serde(rename = "search[user_id]", default)]
    user_id: String,
    #[serde(rename = "search[user_name]", default)]
    user_name: String,
    #[serde(rename = "search[creator_id]", default)]
    creator_id: String,
    #[serde(rename = "search[creator_name]", default)]
    creator_name: String,
    #[serde(rename = "search[category]", default)]
    category: String,
    #[serde(flatten)]
    list: ListParams,
}

/// A user, by id or by name; `Some(None)` when named but not found.
async fn user(state: &AppState, id: &str, name: &str) -> Result<Option<Option<i64>>, AppError> {
    if let Ok(id) = id.trim().parse::<i64>() {
        return Ok(Some(Some(id)));
    }
    if name.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(
        moekura_db::users::by_name(state.db.primary(), name.trim())
            .await?
            .map(|u| u.id),
    ))
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<Params>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let (offset, limit) = window(&params.list, 1000)?;
    let user_id = user(&state, &params.user_id, &params.user_name).await?;
    let creator_id = user(&state, &params.creator_id, &params.creator_name).await?;
    // Someone named who doesn't exist has no feedback.
    if matches!(user_id, Some(None)) || matches!(creator_id, Some(None)) {
        return json(Vec::<DanbooruFeedback>::new(), &params.list.only);
    }
    let category = params.category.trim();
    let filter = Filter {
        user_id: user_id.flatten(),
        creator_id: creator_id.flatten(),
        category: CATEGORIES.into_iter().find(|c| *c == category),
        with_deleted: current.can(Permission::BanUsers),
    };
    if !category.is_empty() && filter.category.is_none() {
        return json(Vec::<DanbooruFeedback>::new(), &params.list.only);
    }
    let list: Vec<DanbooruFeedback> =
        user_feedbacks::list(state.reader(&current), &filter, offset, limit)
            .await?
            .into_iter()
            .map(DanbooruFeedback::from)
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
    let f = user_feedbacks::by_id(state.reader(&current), id)
        .await?
        .filter(|f| !f.is_deleted || current.can(Permission::BanUsers))
        .ok_or(AppError::NotFound)?;
    json(DanbooruFeedback::from(f), "")
}
