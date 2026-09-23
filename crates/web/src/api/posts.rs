//! Posts: search, one post, history.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uwu_core::permissions::Permission;
use uwu_core::search::{Order, Query as SearchQuery};
use uwu_db::media::{self, Asset, Variant};
use uwu_db::posts::{self, Post};
use uwu_db::search::{Count, PageRef, Plan, SearchError};
use uwu_db::{post_versions, tags, users};
use uwu_storage::Key;

use super::absolute_url;
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::{AppError, ErrorBody};
use crate::posts::visibility;

/// A post with its tags and files.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiPost {
    pub id: i64,
    /// `g` (general), `s` (sensitive), `q` (questionable) or `e` (explicit).
    #[schema(example = "s")]
    pub rating: String,
    /// `active`, `pending` (awaiting approval), `flagged` or `deleted`.
    pub status: String,
    pub source: String,
    pub description: String,
    pub parent_id: Option<i64>,
    pub score: i32,
    pub fav_count: i32,
    /// The uploader's name, unless their account is gone.
    pub uploader: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Sorted by name.
    pub tags: Vec<PostTag>,
    pub file: ApiFile,
    /// Smaller renditions (`thumb-<size>`, `sample`, `poster` for videos),
    /// once processing has made them.
    pub variants: Vec<ApiVariant>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PostTag {
    pub name: String,
    /// `general`, `artist`, `copyright`, `character`, `meta`, or a
    /// category the site added.
    pub category: String,
}

/// The original file.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiFile {
    /// On private sites this link expires after an hour or two.
    pub url: String,
    /// `jpeg`, `png`, `gif`, `webp`, `avif`, `jxl`, `mp4` or `webm`.
    pub media_type: String,
    pub width: i32,
    pub height: i32,
    /// In bytes.
    pub size: i64,
    pub duration_ms: Option<i32>,
    /// More than 1 for animations.
    pub frames: i32,
    pub has_audio: bool,
    /// Hex.
    pub sha256: String,
    /// Hex.
    pub md5: String,
    /// Whether thumbnails and the perceptual hash are done.
    pub processed: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiVariant {
    #[schema(example = "thumb-250")]
    pub kind: String,
    pub url: String,
    pub format: String,
    pub width: i32,
    pub height: i32,
    pub size: i64,
}

/// `posts` as API values, in the same order. Posts without a file (never
/// the case outside a failed upload) are left out.
pub(crate) async fn load(
    state: &AppState,
    db: &PgPool,
    posts: Vec<Post>,
) -> sqlx::Result<Vec<ApiPost>> {
    let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();
    let mut assets: HashMap<i64, Asset> = media::for_posts(db, &ids)
        .await?
        .into_iter()
        .map(|a| (a.post_id, a))
        .collect();
    let asset_ids: Vec<i64> = assets.values().map(|a| a.id).collect();
    let mut variants: HashMap<i64, Vec<Variant>> = HashMap::new();
    for variant in media::variants_of(db, &asset_ids).await? {
        variants.entry(variant.asset_id).or_default().push(variant);
    }
    let mut tag_ids: Vec<i32> = posts
        .iter()
        .flat_map(|p| p.tag_ids.iter().copied())
        .collect();
    tag_ids.sort_unstable();
    tag_ids.dedup();
    let categories = tags::categories(db).await?;
    let tag_names: HashMap<i32, PostTagRef> = tags::by_ids(db, &tag_ids)
        .await?
        .into_iter()
        .map(|t| {
            let category = crate::tags::category_name(&categories, t.category_id);
            (t.id, (t.name, category))
        })
        .collect();
    let mut uploader_ids: Vec<i64> = posts.iter().filter_map(|p| p.uploader_id).collect();
    uploader_ids.sort_unstable();
    uploader_ids.dedup();
    let uploaders: HashMap<i64, String> =
        users::names(db, &uploader_ids).await?.into_iter().collect();

    let url = |key: &str| {
        Key::parse(key).map_or_else(String::new, |k| absolute_url(state, &state.file_url(&k)))
    };
    Ok(posts
        .into_iter()
        .filter_map(|post| {
            let asset = assets.remove(&post.id)?;
            let mut tags: Vec<PostTag> = post
                .tag_ids
                .iter()
                .filter_map(|id| tag_names.get(id))
                .map(|(name, category)| PostTag {
                    name: name.clone(),
                    category: category.clone(),
                })
                .collect();
            tags.sort_by(|a, b| a.name.cmp(&b.name));
            let variants = variants
                .remove(&asset.id)
                .unwrap_or_default()
                .into_iter()
                .map(|v| ApiVariant {
                    url: url(&v.storage_key),
                    kind: v.kind,
                    format: v.format,
                    width: v.width,
                    height: v.height,
                    size: v.file_size,
                })
                .collect();
            Some(ApiPost {
                id: post.id,
                rating: post.rating.code().to_owned(),
                status: post.status.as_str().to_owned(),
                uploader: post.uploader_id.and_then(|id| uploaders.get(&id).cloned()),
                source: post.source,
                description: post.description,
                parent_id: post.parent_id,
                score: post.score,
                fav_count: post.fav_count,
                created_at: post.created_at,
                tags,
                file: ApiFile {
                    url: url(&asset.storage_key),
                    sha256: asset.sha256_hex(),
                    md5: hex::encode(&asset.md5),
                    media_type: asset.media_type,
                    width: asset.width,
                    height: asset.height,
                    size: asset.file_size,
                    duration_ms: asset.duration_ms,
                    frames: asset.frames,
                    has_audio: asset.has_audio,
                    processed: asset.processed_at.is_some(),
                },
                variants,
            })
        })
        .collect())
}

/// A tag's name and category name.
type PostTagRef = (String, String);

/// One post, if `current` may see it.
pub(crate) async fn one(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<ApiPost, AppError> {
    current.require(Permission::ViewPosts)?;
    // The primary, so a client sees a post it just changed.
    let db = state.db.primary();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if !visibility(current).allows(&post) {
        return Err(AppError::NotFound);
    }
    load(state, db, vec![post])
        .await?
        .pop()
        .ok_or(AppError::NotFound)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchParams {
    /// A search, written as in the site's search box (`cat -dog
    /// rating:g order:score`).
    #[serde(default)]
    tags: String,
    /// A page number, or a cursor from a previous response's `next` or
    /// `previous`. Cursors keep working however deep the results go;
    /// numbered pages stop at the site's limit.
    #[serde(default)]
    page: String,
    /// Posts per page, up to the site's maximum. Defaults to the site's
    /// page size.
    limit: Option<u32>,
}

/// A page of search results.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PostPage {
    pub posts: Vec<ApiPost>,
    pub count: ApiCount,
    /// `page` for the next page; absent on the last one.
    pub next: Option<String>,
    /// `page` for the previous page; absent on the first one.
    pub previous: Option<String>,
}

/// How many posts match. Large counts are estimated.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiCount {
    pub value: i64,
    /// `exact`, `about` (an estimate) or `at_least` (counting stopped).
    pub accuracy: String,
}

impl From<Count> for ApiCount {
    fn from(count: Count) -> Self {
        let (value, accuracy) = match count {
            Count::Exact(n) => (n, "exact"),
            Count::About(n) => (n, "about"),
            Count::AtLeast(n) => (n, "at_least"),
        };
        Self {
            value,
            accuracy: accuracy.to_owned(),
        }
    }
}

fn search_error(error: SearchError) -> AppError {
    match error {
        SearchError::Invalid(message) => AppError::BadRequest(message),
        SearchError::Db(error) => error.into(),
    }
}

/// Search posts.
///
/// Uses the same syntax as the site (see the search help). Blacklists
/// aren't applied: clients filter as they like. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/posts",
    tag = "posts",
    params(SearchParams),
    responses(
        (status = 200, body = PostPage),
        (status = 400, body = ErrorBody, description = "The search or page isn't valid"),
        (status = 401, body = ErrorBody, description = "The site is private: authenticate"),
    ),
)]
pub(crate) async fn search(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<SearchParams>,
) -> Result<Json<PostPage>, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.db.read();
    let page: PageRef = if params.page.is_empty() {
        PageRef::default()
    } else {
        params
            .page
            .parse()
            .map_err(|()| AppError::BadRequest("`page` must be a number or a cursor".into()))?
    };
    let mut query =
        SearchQuery::parse(params.tags.trim()).map_err(|e| AppError::BadRequest(e.to_string()))?;
    if let Some(limit) = params.limit {
        if limit == 0 {
            return Err(AppError::BadRequest("`limit` must be at least 1".into()));
        }
        query.limit = Some(limit);
    }
    let plan = Plan::resolve(db, &query, &visibility(&current), &state.config.search)
        .await
        .map_err(search_error)?;
    let ids = plan.ids(db, page).await.map_err(search_error)?;
    let count = plan.count(db).await.map_err(search_error)?;

    let mut found = posts::by_ids(db, &ids).await?;
    found.sort_by_key(|p| ids.iter().position(|id| *id == p.id));
    let full = ids.len() == plan.per_page() as usize;
    // Cursors move away from the posts shown: in newest-first order,
    // "next" is lower ids.
    let (towards_end, towards_start) = if plan.order() == Order::IdDesc {
        ('b', 'a')
    } else {
        ('a', 'b')
    };
    let cursor = |direction: char, id: Option<&i64>| id.map(|id| format!("{direction}{id}"));
    let next = if !full {
        None
    } else if plan.supports_keyset() {
        cursor(towards_end, ids.last())
    } else {
        match page {
            PageRef::Number(n) if n < state.config.search.max_page => Some((n + 1).to_string()),
            _ => None,
        }
    };
    let previous = match page {
        PageRef::Number(1) => None,
        PageRef::Number(n) => Some((n - 1).to_string()),
        PageRef::Before(_) | PageRef::After(_) => cursor(towards_start, ids.first()),
    };
    Ok(Json(PostPage {
        posts: load(&state, db, found).await?,
        count: count.into(),
        next,
        previous,
    }))
}

/// Get a post.
///
/// Needs `view_posts`. Pending posts are visible to their uploader and to
/// moderators, deleted ones only to moderators.
#[utoipa::path(
    get,
    path = "/posts/{id}",
    tag = "posts",
    params(("id" = i64, Path, description = "Post number")),
    responses(
        (status = 200, body = ApiPost),
        (status = 404, body = ErrorBody),
    ),
)]
pub(crate) async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<ApiPost>, AppError> {
    Ok(Json(one(&state, &current, id).await?))
}

/// One version of a post.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiVersion {
    /// 1 for the upload, counting up.
    pub version: i32,
    /// Who made the change.
    pub updater: Option<String>,
    /// Set when a tag alias or implication made the change.
    pub relation: Option<VersionRelation>,
    /// All tags after the change, sorted.
    pub tags: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub rating: String,
    pub source: String,
    pub description: String,
    pub parent_id: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct VersionRelation {
    /// `alias` or `implication`.
    pub kind: String,
    pub antecedent: Option<String>,
    pub consequent: Option<String>,
}

/// List a post's history.
///
/// Newest first, at most 500 versions. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/posts/{id}/versions",
    tag = "posts",
    params(("id" = i64, Path, description = "Post number")),
    responses(
        (status = 200, body = Vec<ApiVersion>),
        (status = 404, body = ErrorBody),
    ),
)]
pub(crate) async fn versions(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Vec<ApiVersion>>, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.db.read();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if !visibility(&current).allows(&post) {
        return Err(AppError::NotFound);
    }
    let versions = post_versions::list(db, id).await?;
    let mut ids: Vec<i32> = versions
        .iter()
        .flat_map(|v| v.tag_ids.iter().chain(&v.removed_tag_ids).copied())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let names: HashMap<i32, String> = tags::by_ids(db, &ids)
        .await?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect();
    let named = |ids: &[i32]| {
        let mut list: Vec<String> = ids.iter().filter_map(|id| names.get(id).cloned()).collect();
        list.sort();
        list
    };
    Ok(Json(
        versions
            .into_iter()
            .map(|v| ApiVersion {
                version: v.version,
                tags: named(&v.tag_ids),
                added: named(&v.added_tag_ids),
                removed: named(&v.removed_tag_ids),
                relation: v.relation_kind.map(|kind| VersionRelation {
                    kind,
                    antecedent: v.relation_antecedent,
                    consequent: v.relation_consequent,
                }),
                updater: v.updater_name,
                rating: v.rating,
                source: v.source,
                description: v.description,
                parent_id: v.parent_id,
                created_at: v.created_at,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;
    use sqlx::PgPool;
    use uwu_core::permissions::{Permissions, SystemRole};
    use uwu_db::settings;

    use crate::api::test_support::{app, json, upload};
    use crate::test_support::{fixture, session_for};

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn searches_page_with_cursors(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let mut ids = Vec::new();
        for (n, tags) in ["cat", "cat artist:someone", "cat dog", "dog"]
            .into_iter()
            .enumerate()
        {
            let size = 20 + 2 * n as u32;
            ids.push(upload(&app, &alice, &fixture::png(size, size), tags).await);
        }

        let first = json(&app.get("/api/v1/posts?tags=cat&limit=2", None).await.body);
        let shown: Vec<i64> = first["posts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_i64().unwrap())
            .collect();
        assert_eq!(shown, [ids[2], ids[1]]);
        assert_eq!(first["count"], json!({"value": 3, "accuracy": "exact"}));
        assert_eq!(first["previous"], json!(null));
        let next = first["next"].as_str().unwrap();
        assert_eq!(next, format!("b{}", ids[1]));

        let second = json(
            &app.get(&format!("/api/v1/posts?tags=cat&limit=2&page={next}"), None)
                .await
                .body,
        );
        assert_eq!(second["posts"][0]["id"], json!(ids[0]));
        assert_eq!(second["next"], json!(null));
        assert_eq!(second["previous"], json!(format!("a{}", ids[0])));

        // Numbered pages for orders without cursors.
        let by_score = json(
            &app.get("/api/v1/posts?tags=order:score&limit=3", None)
                .await
                .body,
        );
        assert_eq!(by_score["next"], json!("2"));
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn posts_carry_tags_and_files(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, &fixture::png(32, 24), "cat artist:someone").await;

        let post = json(&app.get(&format!("/api/v1/posts/{id}"), None).await.body);
        assert_eq!(post["id"], json!(id));
        assert_eq!(post["rating"], json!("s"));
        assert_eq!(post["status"], json!("active"));
        assert_eq!(post["uploader"], json!("alice"));
        assert_eq!(
            post["tags"],
            json!([
                {"name": "cat", "category": "general"},
                {"name": "someone", "category": "artist"},
            ])
        );
        let file = &post["file"];
        assert_eq!(
            (&file["width"], &file["height"], &file["media_type"]),
            (&json!(32), &json!(24), &json!("png"))
        );
        assert_eq!(file["processed"], json!(false));
        assert!(
            file["url"]
                .as_str()
                .unwrap()
                .starts_with("http://localhost:8080/data/original/"),
            "{file}"
        );
        assert_eq!(file["sha256"].as_str().unwrap().len(), 64);
        // RFC 3339.
        assert!(post["created_at"].as_str().unwrap().contains('T'));

        let missing = app.get("/api/v1/posts/999999", None).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn pending_posts_stay_hidden_from_others(pool: PgPool) {
        settings::set(&pool, "upload_approval", json!(true))
            .await
            .unwrap();
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let id = upload(&app, &alice, &fixture::png(20, 20), "cat").await;
        let path = format!("/api/v1/posts/{id}");
        assert_eq!(app.get(&path, Some(&alice)).await.status, StatusCode::OK);
        assert_eq!(
            app.get(&path, Some(&bob)).await.status,
            StatusCode::NOT_FOUND
        );
        let search = json(&app.get("/api/v1/posts", Some(&bob)).await.body);
        assert_eq!(search["posts"], json!([]));
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn bad_searches_and_private_sites(pool: PgPool) {
        let app = app(&pool).await;
        let response = app.get("/api/v1/posts?tags=score:abc", None).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(
            json(&response.body)["error"]["message"]
                .as_str()
                .unwrap()
                .contains("score:abc"),
            "{}",
            response.body
        );
        let response = app.get("/api/v1/posts?page=x", None).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);

        sqlx::query("UPDATE roles SET permissions = $1 WHERE system_key = 'anonymous'")
            .bind(Permissions::NONE.to_db())
            .execute(&pool)
            .await
            .unwrap();
        let app = super::super::test_support::app(&pool).await;
        let response = app.get_full("/api/v1/posts").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["www-authenticate"], "Bearer");
        assert!(response.headers().get("location").is_none());
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn history_lists_changes(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, &fixture::png(20, 20), "cat cute").await;
        let form = "old_tags=cat+cute&tags=cat+dog&rating=e";
        let edited = app
            .post_form(&format!("/posts/{id}/edit"), Some(&alice), &[], form)
            .await;
        assert_eq!(edited.status, StatusCode::SEE_OTHER, "{}", edited.body);

        let versions = json(
            &app.get(&format!("/api/v1/posts/{id}/versions"), None)
                .await
                .body,
        );
        let latest = &versions[0];
        assert_eq!(latest["version"], json!(2));
        assert_eq!(latest["updater"], json!("alice"));
        assert_eq!(latest["tags"], json!(["cat", "dog"]));
        assert_eq!(latest["added"], json!(["dog"]));
        assert_eq!(latest["removed"], json!(["cute"]));
        assert_eq!(latest["rating"], json!("e"));
        assert_eq!(versions[1]["added"], json!(["cat", "cute"]));
    }
}
