//! Tags and tag relations.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use moekura_core::permissions::Permission;
use moekura_db::tag_relations::{self, Kind, Relation, Status};
use moekura_db::tags::{self, Category, ListOrder, Tag};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::{AppError, ErrorBody};
use crate::tags::{MAX_PAGE, PAGE_SIZE, Suggestion};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiTag {
    pub id: i32,
    pub name: String,
    pub category: String,
    pub post_count: i32,
    /// Deprecated tags can't be added to posts.
    pub deprecated: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl ApiTag {
    pub(crate) fn new(tag: Tag, categories: &[Category]) -> Self {
        Self {
            category: crate::tags::category_name(categories, tag.category_id),
            id: tag.id,
            name: tag.name,
            post_count: tag.post_count,
            deprecated: tag.is_deprecated,
            created_at: tag.created_at,
        }
    }
}

/// A 1-based page of a list, within the lists' depth limit.
fn page_number(page: Option<i64>) -> Result<i64, AppError> {
    match page.unwrap_or(1) {
        n @ 1..=MAX_PAGE => Ok(n),
        _ => Err(AppError::BadRequest(format!(
            "`page` must be between 1 and {MAX_PAGE}"
        ))),
    }
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TagOrder {
    /// Most used first.
    #[default]
    Count,
    Name,
    Newest,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// A name prefix, or a pattern with `*` wildcards.
    #[serde(default)]
    name: String,
    /// Only tags in this category (`artist`, …).
    #[serde(default)]
    category: String,
    #[serde(default)]
    #[param(inline)]
    order: TagOrder,
    /// 1-based.
    page: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TagPage {
    pub tags: Vec<ApiTag>,
    /// `page` for the next page; absent on the last one.
    pub next_page: Option<i64>,
}

/// List tags.
///
/// 50 per page. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/tags",
    operation_id = "list_tags",
    tag = "tags",
    params(ListParams),
    responses((status = 200, body = TagPage), (status = 400, body = ErrorBody)),
)]
pub(crate) async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<ListParams>,
) -> Result<Json<TagPage>, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let number = page_number(params.page)?;
    let categories = tags::categories(db).await?;
    let category = match params.category.as_str() {
        "" => None,
        name => Some(
            categories
                .iter()
                .find(|c| c.name == name)
                .map(|c| c.id)
                .ok_or_else(|| AppError::BadRequest(format!("There is no category `{name}`")))?,
        ),
    };
    let order = match params.order {
        TagOrder::Count => ListOrder::Count,
        TagOrder::Name => ListOrder::Name,
        TagOrder::Newest => ListOrder::Newest,
    };
    let pattern = moekura_core::tags::normalize(&params.name);
    let mut found = tags::list(
        db,
        &pattern,
        category,
        order,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);
    Ok(Json(TagPage {
        tags: found
            .into_iter()
            .map(|t| ApiTag::new(t, &categories))
            .collect(),
        next_page: has_next.then_some(number + 1),
    }))
}

/// Get a tag.
///
/// Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/tags/{name}",
    operation_id = "get_tag",
    tag = "tags",
    params(("name" = String, Path, description = "The tag's name")),
    responses((status = 200, body = ApiTag), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(name): Path<String>,
) -> Result<Json<ApiTag>, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let tag = tags::by_name(db, &moekura_core::tags::normalize(&name))
        .await?
        .ok_or(AppError::NotFound)?;
    let categories = tags::categories(db).await?;
    Ok(Json(ApiTag::new(tag, &categories)))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AutocompleteParams {
    /// What has been typed of the tag so far.
    #[serde(default)]
    q: String,
}

/// Suggest tags.
///
/// Up to 10 used tags for a prefix, most used first, including tags an
/// alias matching the prefix points to, and similarly spelled tags when
/// few match. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/tags/autocomplete",
    operation_id = "autocomplete_tags",
    tag = "tags",
    params(AutocompleteParams),
    responses(
        (status = 200, body = Vec<Suggestion>),
        (status = 401, body = ErrorBody, description = "The site is private: authenticate"),
    ),
)]
pub(crate) async fn autocomplete(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<AutocompleteParams>,
) -> Result<Json<Vec<Suggestion>>, AppError> {
    current.require(Permission::ViewPosts)?;
    Ok(Json(
        crate::tags::suggestions(state.reader(&current), &params.q).await?,
    ))
}

#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// Posts tagged with the antecedent are tagged with the consequent
    /// instead.
    Alias,
    /// Posts tagged with the antecedent are also tagged with the
    /// consequent.
    Implication,
}

impl From<RelationKind> for Kind {
    fn from(kind: RelationKind) -> Self {
        match kind {
            RelationKind::Alias => Kind::Alias,
            RelationKind::Implication => Kind::Implication,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationStatus {
    Pending,
    Active,
    Rejected,
    Deleted,
}

impl From<RelationStatus> for Status {
    fn from(status: RelationStatus) -> Self {
        match status {
            RelationStatus::Pending => Status::Pending,
            RelationStatus::Active => Status::Active,
            RelationStatus::Rejected => Status::Rejected,
            RelationStatus::Deleted => Status::Deleted,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RelationParams {
    #[param(inline)]
    kind: RelationKind,
    #[param(inline)]
    status: Option<RelationStatus>,
    /// Only relations where either tag starts with this.
    #[serde(default)]
    name: String,
    /// 1-based.
    page: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiRelation {
    pub id: i32,
    /// `alias` or `implication`.
    pub kind: String,
    pub antecedent: String,
    pub consequent: String,
    /// `pending`, `active`, `rejected` or `deleted`.
    pub status: String,
    /// Why it was requested.
    pub reason: String,
    pub creator: Option<String>,
    pub approver: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<Relation> for ApiRelation {
    fn from(r: Relation) -> Self {
        Self {
            id: r.id,
            kind: r.kind.as_str().to_owned(),
            antecedent: r.antecedent,
            consequent: r.consequent,
            status: r.status.as_str().to_owned(),
            reason: r.reason,
            creator: r.creator_name,
            approver: r.approver_name,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RelationPage {
    pub relations: Vec<ApiRelation>,
    /// `page` for the next page; absent on the last one.
    pub next_page: Option<i64>,
}

/// List tag aliases or implications.
///
/// Newest first, 50 per page. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/tag-relations",
    operation_id = "list_tag_relations",
    tag = "tags",
    params(RelationParams),
    responses((status = 200, body = RelationPage), (status = 400, body = ErrorBody)),
)]
pub(crate) async fn relations(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<RelationParams>,
) -> Result<Json<RelationPage>, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let number = page_number(params.page)?;
    let mut found = tag_relations::list(
        db,
        params.kind.into(),
        params.status.map(Status::from),
        &moekura_core::tags::normalize(&params.name),
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);
    Ok(Json(RelationPage {
        relations: found.into_iter().map(ApiRelation::from).collect(),
        next_page: has_next.then_some(number + 1),
    }))
}

/// Changes to a tag. Fields left out stay as they are.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TagChanges {
    /// The category's name (`general`, `artist`, …).
    category: Option<String>,
    /// Deprecated tags can't be added to posts.
    deprecated: Option<bool>,
}

/// Edit a tag.
///
/// Needs `manage_tags`. The change is recorded in the moderation log.
#[utoipa::path(
    patch,
    path = "/tags/{name}",
    operation_id = "update_tag",
    tag = "tags",
    params(("name" = String, Path, description = "The tag's name")),
    request_body = TagChanges,
    responses(
        (status = 200, body = ApiTag),
        (status = 404, body = ErrorBody),
        (status = 422, body = ErrorBody, description = "There is no such category"),
    ),
)]
pub(crate) async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(name): Path<String>,
    Json(changes): Json<TagChanges>,
) -> Result<Json<ApiTag>, AppError> {
    current.require(Permission::ManageTags)?;
    let db = state.db.primary();
    let tag = tags::by_name(db, &moekura_core::tags::normalize(&name))
        .await?
        .ok_or(AppError::NotFound)?;
    let categories = tags::categories(db).await?;
    let category = match &changes.category {
        None => tag.category_id,
        Some(name) => categories
            .iter()
            .find(|c| &c.name == name)
            .map(|c| c.id)
            .ok_or_else(|| AppError::Unprocessable(format!("There is no category `{name}`")))?,
    };
    let deprecated = changes.deprecated.unwrap_or(tag.is_deprecated);
    crate::tags::update(&state, &current, &tag, category, deprecated).await?;
    let tag = tags::by_id(db, tag.id).await?.ok_or(AppError::NotFound)?;
    Ok(Json(ApiTag::new(tag, &categories)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewRelation {
    #[schema(inline)]
    kind: RelationKind,
    /// The tag that's aliased, or that implies the other.
    antecedent: String,
    /// The tag it's aliased to, or that it implies.
    consequent: String,
    /// Why, for whoever reviews the request.
    #[serde(default)]
    reason: String,
}

/// Request an alias or implication.
///
/// Needs `edit_posts`. Requests wait for someone with `manage_tags`,
/// whose own requests take effect at once; either way the relation is then
/// applied to existing posts in the background.
#[utoipa::path(
    post,
    path = "/tag-relations",
    operation_id = "request_tag_relation",
    tag = "tags",
    request_body = NewRelation,
    responses(
        (status = 201, body = ApiRelation),
        (status = 422, body = ErrorBody, description = "The tags aren't valid, or the relation would conflict with others"),
    ),
)]
pub(crate) async fn request(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(new): Json<NewRelation>,
) -> Result<(StatusCode, Json<ApiRelation>), AppError> {
    let id = crate::tag_relations::request_relation(
        &state,
        &current,
        new.kind.into(),
        &new.antecedent,
        &new.consequent,
        &new.reason,
    )
    .await?;
    let relation = tag_relations::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, Json(relation.into())))
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
    async fn lists_and_finds_tags(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        upload(
            &app,
            &alice,
            &fixture::png(20, 20),
            "cat cat_ears artist:someone",
        )
        .await;
        upload(&app, &alice, &fixture::png(22, 20), "cat").await;

        let page = json(&app.get("/api/v1/tags?name=cat", None).await.body);
        let names: Vec<&str> = page["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["cat", "cat_ears"]);
        assert_eq!(page["tags"][0]["post_count"], json!(2));
        assert_eq!(page["next_page"], json!(null));

        let artists = json(&app.get("/api/v1/tags?category=artist", None).await.body);
        assert_eq!(artists["tags"][0]["name"], json!("someone"));
        let unknown = app.get("/api/v1/tags?category=nope", None).await;
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
        let too_deep = app.get("/api/v1/tags?page=0", None).await;
        assert_eq!(too_deep.status, StatusCode::BAD_REQUEST);

        let tag = json(&app.get("/api/v1/tags/Someone", None).await.body);
        assert_eq!(
            (&tag["name"], &tag["category"]),
            (&json!("someone"), &json!("artist"))
        );
        assert_eq!(
            app.get("/api/v1/tags/nope", None).await.status,
            StatusCode::NOT_FOUND
        );

        let suggestions = json(&app.get("/api/v1/tags/autocomplete?q=ca", None).await.body);
        assert_eq!(suggestions[0]["name"], json!("cat"));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn lists_relations(pool: PgPool) {
        let app = app(&pool).await;
        session_for(&pool, "alice", SystemRole::Member).await;
        let alice_id = moekura_db::users::by_name(&pool, "alice")
            .await
            .unwrap()
            .unwrap()
            .id;
        moekura_db::tag_relations::request(
            &pool,
            moekura_db::tag_relations::NewRequest {
                kind: moekura_db::tag_relations::Kind::Alias,
                antecedent: "kitty",
                consequent: "cat",
                reason: "same thing",
                creator_id: Some(alice_id),
            },
        )
        .await
        .unwrap();

        let page = json(&app.get("/api/v1/tag-relations?kind=alias", None).await.body);
        let relation = &page["relations"][0];
        assert_eq!(
            (
                &relation["antecedent"],
                &relation["consequent"],
                &relation["status"]
            ),
            (&json!("kitty"), &json!("cat"), &json!("pending"))
        );
        assert_eq!(relation["creator"], json!("alice"));
        let none = json(
            &app.get("/api/v1/tag-relations?kind=implication", None)
                .await
                .body,
        );
        assert_eq!(none["relations"], json!([]));
        let missing_kind = app.get("/api/v1/tag-relations", None).await;
        assert_eq!(missing_kind.status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn edits_tags_and_requests_relations(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        upload(&app, &alice, &fixture::png(20, 20), "someone").await;

        let change = json!({"category": "artist", "deprecated": false});
        let refused = app
            .json(
                "PATCH",
                "/api/v1/tags/someone",
                Some(&alice),
                Some(change.clone()),
            )
            .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        let tag = json(
            &app.json("PATCH", "/api/v1/tags/someone", Some(&admin), Some(change))
                .await
                .body,
        );
        assert_eq!(tag["category"], json!("artist"));
        let unknown = app
            .json(
                "PATCH",
                "/api/v1/tags/someone",
                Some(&admin),
                Some(json!({"category": "nope"})),
            )
            .await;
        assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);

        let request =
            json!({"kind": "alias", "antecedent": "kitty", "consequent": "cat", "reason": "same"});
        let pending = app
            .json("POST", "/api/v1/tag-relations", Some(&alice), Some(request))
            .await;
        assert_eq!(pending.status, StatusCode::CREATED, "{}", pending.body);
        assert_eq!(json(&pending.body)["status"], json!("pending"));
        let active = json(
            &app.json(
                "POST",
                "/api/v1/tag-relations",
                Some(&admin),
                Some(json!({"kind": "implication", "antecedent": "cat", "consequent": "animal"})),
            )
            .await
            .body,
        );
        assert_eq!(active["status"], json!("active"));
        let same = app
            .json(
                "POST",
                "/api/v1/tag-relations",
                Some(&alice),
                Some(json!({"kind": "alias", "antecedent": "a", "consequent": "a"})),
            )
            .await;
        assert_eq!(same.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(same.body.contains("two different tags"), "{}", same.body);
    }
}
