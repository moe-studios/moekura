//! Pools and their history.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use moekura_core::markup;
use moekura_core::permissions::Permission;
use moekura_core::pools::Category;
use moekura_db::pools::{self, Pool, Version};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::{AppError, ErrorBody};
use crate::pools::{PoolInput, append, contents, save_error, set_deleted, visible_pool};
use crate::posts::visibility;
use crate::tags::{MAX_PAGE, PAGE_SIZE};

use super::tags::page_number;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiPool {
    pub id: i32,
    /// Spaces are underscores.
    pub name: String,
    /// In the site's wiki markup.
    pub description: String,
    /// `series` or `collection`.
    pub category: String,
    pub is_deleted: bool,
    /// Counts up with every change; send it back when saving.
    pub version: i32,
    pub updater: Option<String>,
    /// All its posts, including those you can't see.
    pub post_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<Pool> for ApiPool {
    fn from(p: Pool) -> Self {
        Self {
            id: p.id,
            name: p.name,
            description: p.description,
            category: p.category,
            is_deleted: p.is_deleted,
            version: p.version,
            updater: p.updater_name,
            post_count: p.post_count,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiPoolWithPosts {
    #[serde(flatten)]
    pub pool: ApiPool,
    /// `description` as HTML.
    pub html: String,
    /// The posts you can see, in order.
    pub post_ids: Vec<i64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PoolList {
    pub pools: Vec<ApiPool>,
    /// `page` for the next page; absent on the last one.
    pub next_page: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// A name prefix, or a pattern with `*` wildcards.
    #[serde(default)]
    name: String,
    /// `series` or `collection`.
    category: Option<String>,
    /// 1-based.
    page: Option<i64>,
}

/// List pools.
///
/// Most recently changed first, 50 per page. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/pools",
    operation_id = "list_pools",
    tag = "pools",
    params(ListParams),
    responses((status = 200, body = PoolList), (status = 400, body = ErrorBody)),
)]
pub(crate) async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<ListParams>,
) -> Result<Json<PoolList>, AppError> {
    current.require(Permission::ViewPosts)?;
    let number = page_number(params.page)?;
    let name = params.name.split_whitespace().collect::<Vec<_>>().join("_");
    let filter = pools::Filter {
        name: &name,
        category: params
            .category
            .as_deref()
            .and_then(Category::parse)
            .map(Category::as_str),
        with_deleted: current.can(Permission::DeletePosts) || current.can(Permission::ViewDeleted),
    };
    let mut found = pools::list(
        state.reader(&current),
        &filter,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);
    Ok(Json(PoolList {
        pools: found.into_iter().map(ApiPool::from).collect(),
        next_page: has_next.then_some(number + 1),
    }))
}

async fn with_posts(
    state: &AppState,
    current: &CurrentUser,
    pool: Pool,
) -> Result<ApiPoolWithPosts, AppError> {
    let post_ids = pools::visible_post_ids(
        state.db.primary(),
        pool.id,
        &visibility(current),
        0,
        i64::MAX,
    )
    .await?;
    Ok(ApiPoolWithPosts {
        html: markup::render(&pool.description),
        pool: pool.into(),
        post_ids,
    })
}

/// Get a pool.
///
/// Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/pools/{id}",
    operation_id = "get_pool",
    tag = "pools",
    params(("id" = i32, Path, description = "Pool number")),
    responses((status = 200, body = ApiPoolWithPosts), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
) -> Result<Json<ApiPoolWithPosts>, AppError> {
    let pool = visible_pool(state.reader(&current), &current, id).await?;
    Ok(Json(with_posts(&state, &current, pool).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPool {
    name: String,
    #[serde(default)]
    description: String,
    /// `series` (the default) or `collection`.
    #[serde(default)]
    category: Option<String>,
    /// In order.
    #[serde(default)]
    post_ids: Vec<i64>,
}

fn dedup(ids: Vec<i64>) -> Vec<i64> {
    let mut seen = Vec::with_capacity(ids.len());
    for id in ids {
        if !seen.contains(&id) {
            seen.push(id);
        }
    }
    seen
}

/// Create a pool.
///
/// Needs `edit_pools`.
#[utoipa::path(
    post,
    path = "/pools",
    operation_id = "create_pool",
    tag = "pools",
    request_body = NewPool,
    responses(
        (status = 201, body = ApiPoolWithPosts),
        (status = 422, body = ErrorBody, description = "A bad or taken name, or posts that don't exist"),
    ),
)]
pub(crate) async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(new): Json<NewPool>,
) -> Result<(StatusCode, Json<ApiPoolWithPosts>), AppError> {
    current.require(Permission::EditPools)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let contents = contents(
        &PoolInput {
            name: &new.name,
            description: &new.description,
            category: new.category.as_deref().unwrap_or("series"),
            post_ids: dedup(new.post_ids),
        },
        false,
    )?;
    let db = state.db.primary();
    let id = pools::create(db, &contents, Some(user.id))
        .await
        .map_err(save_error)?;
    let pool = pools::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    Ok((
        StatusCode::CREATED,
        Json(with_posts(&state, &current, pool).await?),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PoolChanges {
    name: Option<String>,
    description: Option<String>,
    category: Option<String>,
    /// All of the pool's posts, in order.
    post_ids: Option<Vec<i64>>,
    /// The version the changes were based on. If someone has changed the
    /// pool since, the save is refused with 409. Leave it out to save over
    /// whatever is there.
    base_version: Option<i32>,
}

/// Change a pool.
///
/// Needs `edit_pools`. Fields left out stay as they are.
#[utoipa::path(
    put,
    path = "/pools/{id}",
    operation_id = "update_pool",
    tag = "pools",
    params(("id" = i32, Path, description = "Pool number")),
    request_body = PoolChanges,
    responses(
        (status = 200, body = ApiPoolWithPosts),
        (status = 409, body = ErrorBody, description = "The pool changed since `base_version`"),
        (status = 422, body = ErrorBody),
    ),
)]
pub(crate) async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
    Json(changes): Json<PoolChanges>,
) -> Result<Json<ApiPoolWithPosts>, AppError> {
    current.require(Permission::EditPools)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = state.db.primary();
    let pool = visible_pool(db, &current, id).await?;
    let now = pools::contents(db, id).await?.ok_or(AppError::NotFound)?;
    let contents = contents(
        &PoolInput {
            name: changes.name.as_deref().unwrap_or(&now.name),
            description: changes.description.as_deref().unwrap_or(&now.description),
            category: changes.category.as_deref().unwrap_or(&now.category),
            post_ids: dedup(changes.post_ids.unwrap_or(now.post_ids)),
        },
        pool.is_deleted,
    )?;
    pools::save(db, id, &contents, Some(user.id), changes.base_version)
        .await
        .map_err(save_error)?;
    let pool = pools::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    Ok(Json(with_posts(&state, &current, pool).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PoolPost {
    post_id: i64,
}

/// Add a post to the end of a pool.
///
/// Needs `edit_pools`.
#[utoipa::path(
    post,
    path = "/pools/{id}/posts",
    operation_id = "add_pool_post",
    tag = "pools",
    params(("id" = i32, Path, description = "Pool number")),
    request_body = PoolPost,
    responses(
        (status = 200, body = ApiPoolWithPosts),
        (status = 422, body = ErrorBody, description = "Already in the pool, deleted, or the pool is full"),
    ),
)]
pub(crate) async fn add_post(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
    Json(post): Json<PoolPost>,
) -> Result<Json<ApiPoolWithPosts>, AppError> {
    let db = state.db.primary();
    let pool = visible_pool(db, &current, id).await?;
    append(&state, &current, &pool, post.post_id).await?;
    let pool = pools::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    Ok(Json(with_posts(&state, &current, pool).await?))
}

/// Delete a pool.
///
/// Needs `delete_posts`; logged. It can be restored.
#[utoipa::path(
    delete,
    path = "/pools/{id}",
    operation_id = "delete_pool",
    tag = "pools",
    params(("id" = i32, Path, description = "Pool number")),
    responses((status = 204), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn delete(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
) -> Result<StatusCode, AppError> {
    set_deleted(&state, &current, id, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Restore a deleted pool.
///
/// Needs `delete_posts`; logged.
#[utoipa::path(
    post,
    path = "/pools/{id}/restore",
    operation_id = "restore_pool",
    tag = "pools",
    params(("id" = i32, Path, description = "Pool number")),
    responses((status = 204), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn restore(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
) -> Result<StatusCode, AppError> {
    set_deleted(&state, &current, id, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiPoolVersion {
    pub version: i32,
    pub updater: Option<String>,
    pub name: String,
    pub description: String,
    pub category: String,
    pub is_deleted: bool,
    pub post_ids: Vec<i64>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<Version> for ApiPoolVersion {
    fn from(v: Version) -> Self {
        Self {
            version: v.version,
            updater: v.updater_name,
            name: v.name,
            description: v.description,
            category: v.category,
            is_deleted: v.is_deleted,
            post_ids: v.post_ids,
            created_at: v.created_at,
        }
    }
}

/// A pool's history.
///
/// Every version, newest first (at most 500). Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/pools/{id}/versions",
    operation_id = "list_pool_versions",
    tag = "pools",
    params(("id" = i32, Path, description = "Pool number")),
    responses((status = 200, body = Vec<ApiPoolVersion>), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn versions(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
) -> Result<Json<Vec<ApiPoolVersion>>, AppError> {
    let db = state.reader(&current);
    visible_pool(db, &current, id).await?;
    Ok(Json(
        pools::versions(db, id)
            .await?
            .into_iter()
            .map(ApiPoolVersion::from)
            .collect(),
    ))
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
    async fn pools(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        let a = upload(&app, &jan, &fixture::png(20, 20), "cat").await;
        let b = upload(&app, &jan, &fixture::png(24, 20), "dog").await;

        let created = app
            .json(
                "POST",
                "/api/v1/pools",
                Some(&alice),
                Some(json!({ "name": "My Comic", "post_ids": [b, a, b] })),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let body = json(&created.body);
        let id = body["id"].as_i64().unwrap();
        assert_eq!(body["name"], json!("My_Comic"));
        assert_eq!(body["category"], json!("series"));
        assert_eq!(body["post_ids"], json!([b, a]));

        let stale = app
            .json(
                "PUT",
                &format!("/api/v1/pools/{id}"),
                Some(&alice),
                Some(json!({ "category": "collection", "base_version": 0 })),
            )
            .await;
        assert_eq!(stale.status, StatusCode::CONFLICT);
        let changed = app
            .json(
                "PUT",
                &format!("/api/v1/pools/{id}"),
                Some(&alice),
                Some(json!({ "post_ids": [a], "base_version": 1 })),
            )
            .await;
        assert_eq!(json(&changed.body)["version"], json!(2));
        let added = app
            .json(
                "POST",
                &format!("/api/v1/pools/{id}/posts"),
                Some(&alice),
                Some(json!({ "post_id": b })),
            )
            .await;
        assert_eq!(json(&added.body)["post_ids"], json!([a, b]));

        let list = json(&app.get("/api/v1/pools?name=my", None).await.body);
        assert_eq!(list["pools"][0]["id"], json!(id));
        let versions = json(
            &app.get(&format!("/api/v1/pools/{id}/versions"), None)
                .await
                .body,
        );
        assert_eq!(versions.as_array().unwrap().len(), 3);

        assert_eq!(
            app.json("DELETE", &format!("/api/v1/pools/{id}"), Some(&alice), None)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.json("DELETE", &format!("/api/v1/pools/{id}"), Some(&jan), None)
                .await
                .status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            app.get(&format!("/api/v1/pools/{id}"), None).await.status,
            StatusCode::NOT_FOUND
        );
    }
}
