//! `/news_updates.json` and `/news_updates/{id}.json`: site news, newest
//! first, deleted news left out.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::news::{self, NewsUpdate};
use serde::Serialize;

use super::tags::window;
use super::{ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/news_updates", get(index))
        .route("/news_updates/{id}", get(show))
}

#[derive(Debug, Serialize)]
struct DanbooruNews {
    id: i64,
    creator_id: Option<i64>,
    updater_id: Option<i64>,
    message: String,
    created_at: String,
    updated_at: String,
}

impl From<NewsUpdate> for DanbooruNews {
    fn from(n: NewsUpdate) -> Self {
        Self {
            id: n.id,
            creator_id: n.creator_id,
            updater_id: n.creator_id,
            message: n.body,
            created_at: timestamp(n.created_at),
            updated_at: timestamp(n.updated_at),
        }
    }
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<ListParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let (offset, limit) = window(&params, 1000)?;
    let list: Vec<DanbooruNews> = news::list(state.reader(&current), false, offset, limit)
        .await?
        .into_iter()
        .map(DanbooruNews::from)
        .collect();
    json(list, &params.only)
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let found = news::by_id(state.reader(&current), id)
        .await?
        .filter(|n| !n.is_deleted)
        .ok_or(AppError::NotFound)?;
    json(DanbooruNews::from(found), "")
}
