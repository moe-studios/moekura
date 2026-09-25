//! The requester's saved searches.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use moekura_db::saved_searches::{self, SavedSearch};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::{AppError, ErrorBody};
use crate::saved_searches::{clean_labels, clean_query, save, update_error};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiSavedSearch {
    pub id: i64,
    /// Normalised.
    pub query: String,
    pub labels: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<SavedSearch> for ApiSavedSearch {
    fn from(s: SavedSearch) -> Self {
        Self {
            id: s.id,
            query: s.query,
            labels: s.labels,
            created_at: s.created_at,
        }
    }
}

fn me(current: &CurrentUser) -> Result<i64, AppError> {
    current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)
}

/// Your saved searches.
///
/// By query. Search them together with `search:all` or `search:<label>`.
#[utoipa::path(
    get,
    path = "/saved-searches",
    operation_id = "list_saved_searches",
    tag = "users",
    responses((status = 200, body = Vec<ApiSavedSearch>), (status = 401, body = ErrorBody)),
)]
pub(crate) async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
) -> Result<Json<Vec<ApiSavedSearch>>, AppError> {
    let user = me(&current)?;
    Ok(Json(
        saved_searches::for_user(state.db.primary(), user)
            .await?
            .into_iter()
            .map(ApiSavedSearch::from)
            .collect(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SavedSearchInput {
    query: String,
    /// Letters, digits, `_` and `-`; at most 10.
    #[serde(default)]
    labels: Vec<String>,
}

async fn one(state: &AppState, user: i64, id: i64) -> Result<ApiSavedSearch, AppError> {
    saved_searches::for_user(state.db.primary(), user)
        .await?
        .into_iter()
        .find(|s| s.id == id)
        .map(ApiSavedSearch::from)
        .ok_or(AppError::NotFound)
}

/// Save a search.
///
/// Saving a search you already saved replaces its labels. At most 100.
#[utoipa::path(
    post,
    path = "/saved-searches",
    operation_id = "create_saved_search",
    tag = "users",
    request_body = SavedSearchInput,
    responses((status = 200, body = ApiSavedSearch), (status = 422, body = ErrorBody)),
)]
pub(crate) async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(input): Json<SavedSearchInput>,
) -> Result<Json<ApiSavedSearch>, AppError> {
    let user = me(&current)?;
    let id = save(&state, &current, &input.query, &input.labels.join(" ")).await?;
    Ok(Json(one(&state, user, id).await?))
}

/// Change a saved search.
#[utoipa::path(
    put,
    path = "/saved-searches/{id}",
    operation_id = "update_saved_search",
    tag = "users",
    params(("id" = i64, Path, description = "Saved search number")),
    request_body = SavedSearchInput,
    responses((status = 200, body = ApiSavedSearch), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)),
)]
pub(crate) async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(input): Json<SavedSearchInput>,
) -> Result<Json<ApiSavedSearch>, AppError> {
    let user = me(&current)?;
    let query = clean_query(&input.query)?;
    let labels = clean_labels(&input.labels.join(" "))?;
    if !saved_searches::update(state.db.primary(), user, id, &query, &labels)
        .await
        .map_err(update_error)?
    {
        return Err(AppError::NotFound);
    }
    Ok(Json(one(&state, user, id).await?))
}

/// Remove a saved search.
#[utoipa::path(
    delete,
    path = "/saved-searches/{id}",
    operation_id = "delete_saved_search",
    tag = "users",
    params(("id" = i64, Path, description = "Saved search number")),
    responses((status = 204), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn delete(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let user = me(&current)?;
    if !saved_searches::remove(state.db.primary(), user, id).await? {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::json;
    use sqlx::PgPool;

    use crate::api::test_support::{app, json};
    use crate::test_support::session_for;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn saved_searches(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        assert_eq!(
            app.get("/api/v1/saved-searches", None).await.status,
            StatusCode::UNAUTHORIZED
        );
        let saved = app
            .json(
                "POST",
                "/api/v1/saved-searches",
                Some(&alice),
                Some(json!({ "query": "Cat  rating:G", "labels": ["Pets"] })),
            )
            .await;
        assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
        let body = json(&saved.body);
        assert_eq!(
            (&body["query"], &body["labels"]),
            (&json!("cat rating:g"), &json!(["pets"]))
        );
        let id = body["id"].as_i64().unwrap();
        let changed = app
            .json(
                "PUT",
                &format!("/api/v1/saved-searches/{id}"),
                Some(&alice),
                Some(json!({ "query": "dog" })),
            )
            .await;
        assert_eq!(json(&changed.body)["labels"], json!([]));
        let bad = app
            .json(
                "POST",
                "/api/v1/saved-searches",
                Some(&alice),
                Some(json!({ "query": "search:all" })),
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json(&app.get("/api/v1/saved-searches", Some(&alice)).await.body)
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            app.json(
                "DELETE",
                &format!("/api/v1/saved-searches/{id}"),
                Some(&alice),
                None
            )
            .await
            .status,
            StatusCode::NO_CONTENT
        );
    }
}
