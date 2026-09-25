//! Favorite groups.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use moekura_core::permissions::Permission;
use moekura_db::favorite_groups::{self, Group};
use moekura_db::users;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::{AppError, ErrorBody};
use crate::favorite_groups::{MAX_GROUPS, append, contents, own_group, save_error, visible_group};
use crate::posts::visibility;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiFavoriteGroup {
    pub id: i32,
    pub creator: String,
    /// Spaces are underscores.
    pub name: String,
    pub is_public: bool,
    /// All its posts, including those you can't see.
    pub post_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<Group> for ApiFavoriteGroup {
    fn from(g: Group) -> Self {
        Self {
            id: g.id,
            creator: g.creator_name,
            name: g.name,
            is_public: g.is_public,
            post_count: g.post_count,
            created_at: g.created_at,
            updated_at: g.updated_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiFavoriteGroupWithPosts {
    #[serde(flatten)]
    pub group: ApiFavoriteGroup,
    /// The posts you can see, in order.
    pub post_ids: Vec<i64>,
}

async fn with_posts(
    state: &AppState,
    current: &CurrentUser,
    group: Group,
) -> Result<ApiFavoriteGroupWithPosts, AppError> {
    let post_ids = favorite_groups::visible_post_ids(
        state.db.primary(),
        group.id,
        &visibility(current),
        0,
        i64::MAX,
    )
    .await?;
    Ok(ApiFavoriteGroupWithPosts {
        group: group.into(),
        post_ids,
    })
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// Whose groups (a name); yours when left out.
    user: Option<String>,
}

/// List a user's favorite groups.
///
/// Their public groups by name, or all of yours. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/favorite-groups",
    operation_id = "list_favorite_groups",
    tag = "users",
    params(ListParams),
    responses((status = 200, body = Vec<ApiFavoriteGroup>), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<ApiFavoriteGroup>>, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let me = current.user.as_ref().map(|u| u.id);
    let owner = match params.user {
        Some(name) => {
            users::by_name(db, &name)
                .await?
                .ok_or(AppError::NotFound)?
                .id
        }
        None => me.ok_or(AppError::Unauthorized)?,
    };
    Ok(Json(
        favorite_groups::for_user(db, owner, me == Some(owner))
            .await?
            .into_iter()
            .map(ApiFavoriteGroup::from)
            .collect(),
    ))
}

/// Get a favorite group.
///
/// Public groups, and your own private ones. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/favorite-groups/{id}",
    operation_id = "get_favorite_group",
    tag = "users",
    params(("id" = i32, Path, description = "Group number")),
    responses((status = 200, body = ApiFavoriteGroupWithPosts), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
) -> Result<Json<ApiFavoriteGroupWithPosts>, AppError> {
    let group = visible_group(state.reader(&current), &current, id).await?;
    Ok(Json(with_posts(&state, &current, group).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupInput {
    name: Option<String>,
    /// Public (the default) or private.
    is_public: Option<bool>,
    /// All of its posts, in order.
    post_ids: Option<Vec<i64>>,
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

/// Create a favorite group.
///
/// Needs `favorite`; at most 100 each.
#[utoipa::path(
    post,
    path = "/favorite-groups",
    operation_id = "create_favorite_group",
    tag = "users",
    request_body = GroupInput,
    responses((status = 201, body = ApiFavoriteGroupWithPosts), (status = 422, body = ErrorBody)),
)]
pub(crate) async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(input): Json<GroupInput>,
) -> Result<(StatusCode, Json<ApiFavoriteGroupWithPosts>), AppError> {
    current.require(Permission::Favorite)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = state.db.primary();
    let contents = contents(
        input.name.as_deref().unwrap_or_default(),
        input.is_public.unwrap_or(true),
        dedup(input.post_ids.unwrap_or_default()),
    )?;
    if favorite_groups::count_for_user(db, user.id).await? >= MAX_GROUPS {
        return Err(AppError::Unprocessable(format!(
            "You can have at most {MAX_GROUPS} groups."
        )));
    }
    let id = favorite_groups::create(db, user.id, &contents)
        .await
        .map_err(save_error)?;
    let group = favorite_groups::by_id(db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((
        StatusCode::CREATED,
        Json(with_posts(&state, &current, group).await?),
    ))
}

/// Change your favorite group.
///
/// Fields left out stay as they are.
#[utoipa::path(
    put,
    path = "/favorite-groups/{id}",
    operation_id = "update_favorite_group",
    tag = "users",
    params(("id" = i32, Path, description = "Group number")),
    request_body = GroupInput,
    responses((status = 200, body = ApiFavoriteGroupWithPosts), (status = 403, body = ErrorBody), (status = 422, body = ErrorBody)),
)]
pub(crate) async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
    Json(input): Json<GroupInput>,
) -> Result<Json<ApiFavoriteGroupWithPosts>, AppError> {
    let db = state.db.primary();
    let group = own_group(db, &current, id).await?;
    let post_ids = match input.post_ids {
        Some(ids) => dedup(ids),
        None => favorite_groups::post_ids(db, id).await?,
    };
    let contents = contents(
        input.name.as_deref().unwrap_or(&group.name),
        input.is_public.unwrap_or(group.is_public),
        post_ids,
    )?;
    favorite_groups::save(db, id, &contents)
        .await
        .map_err(save_error)?;
    let group = favorite_groups::by_id(db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(with_posts(&state, &current, group).await?))
}

/// Delete your favorite group.
#[utoipa::path(
    delete,
    path = "/favorite-groups/{id}",
    operation_id = "delete_favorite_group",
    tag = "users",
    params(("id" = i32, Path, description = "Group number")),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn delete(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
) -> Result<StatusCode, AppError> {
    let db = state.db.primary();
    own_group(db, &current, id).await?;
    favorite_groups::delete(db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupPost {
    post_id: i64,
}

/// Add a post to the end of your favorite group.
///
/// Adding a post that's already there changes nothing.
#[utoipa::path(
    post,
    path = "/favorite-groups/{id}/posts",
    operation_id = "add_favorite_group_post",
    tag = "users",
    params(("id" = i32, Path, description = "Group number")),
    request_body = GroupPost,
    responses((status = 200, body = ApiFavoriteGroupWithPosts), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn add_post(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
    Json(post): Json<GroupPost>,
) -> Result<Json<ApiFavoriteGroupWithPosts>, AppError> {
    append(&state, &current, id, post.post_id).await?;
    let group = favorite_groups::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(with_posts(&state, &current, group).await?))
}

/// Take a post out of your favorite group.
#[utoipa::path(
    delete,
    path = "/favorite-groups/{id}/posts/{post_id}",
    operation_id = "remove_favorite_group_post",
    tag = "users",
    params(
        ("id" = i32, Path, description = "Group number"),
        ("post_id" = i64, Path, description = "Post number"),
    ),
    responses((status = 204), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn remove_post(
    State(state): State<AppState>,
    current: CurrentUser,
    Path((id, post_id)): Path<(i32, i64)>,
) -> Result<StatusCode, AppError> {
    let db = state.db.primary();
    own_group(db, &current, id).await?;
    if !favorite_groups::remove_post(db, id, post_id).await? {
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

    use crate::api::test_support::{app, json, upload};
    use crate::test_support::{fixture, session_for};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn favorite_groups(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        let a = upload(&app, &jan, &fixture::png(20, 20), "cat").await;

        let created = app
            .json(
                "POST",
                "/api/v1/favorite-groups",
                Some(&alice),
                Some(json!({ "name": "Best", "is_public": false })),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let id = json(&created.body)["id"].as_i64().unwrap();
        let added = app
            .json(
                "POST",
                &format!("/api/v1/favorite-groups/{id}/posts"),
                Some(&alice),
                Some(json!({ "post_id": a })),
            )
            .await;
        assert_eq!(json(&added.body)["post_ids"], json!([a]));
        assert_eq!(
            app.get(&format!("/api/v1/favorite-groups/{id}"), Some(&bob))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        assert!(
            json(
                &app.get("/api/v1/favorite-groups?user=alice", Some(&bob))
                    .await
                    .body
            )
            .as_array()
            .unwrap()
            .is_empty()
        );
        app.json(
            "PUT",
            &format!("/api/v1/favorite-groups/{id}"),
            Some(&alice),
            Some(json!({ "is_public": true })),
        )
        .await;
        let shown = json(
            &app.get(&format!("/api/v1/favorite-groups/{id}"), Some(&bob))
                .await
                .body,
        );
        assert_eq!(
            (&shown["creator"], &shown["post_ids"]),
            (&json!("alice"), &json!([a]))
        );
        assert_eq!(
            app.json(
                "DELETE",
                &format!("/api/v1/favorite-groups/{id}/posts/{a}"),
                Some(&bob),
                None
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.json(
                "DELETE",
                &format!("/api/v1/favorite-groups/{id}/posts/{a}"),
                Some(&alice),
                None
            )
            .await
            .status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            app.json(
                "DELETE",
                &format!("/api/v1/favorite-groups/{id}"),
                Some(&alice),
                None
            )
            .await
            .status,
            StatusCode::NO_CONTENT
        );
    }
}
