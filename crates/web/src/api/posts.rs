//! Posts: search, one post, history.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::header::LOCATION;
use axum::http::{HeaderName, StatusCode};
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
use crate::edit::{EditForm, Refused};
use crate::error::{AppError, ErrorBody};
use crate::favorites::{self, Reactions};
use crate::posts::visibility;
use crate::upload::UploadError;

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
    operation_id = "search_posts",
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
    let db = state.reader(&current);
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
    let count = state
        .counts
        .count(&plan, db, &current)
        .await
        .map_err(search_error)?;

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
    operation_id = "get_post",
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
    operation_id = "list_post_versions",
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
    let db = state.reader(&current);
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

/// The fields of an upload, for the description only: the handler reads
/// the multipart body as the upload form does.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct UploadRequest {
    /// The file. Send either this or `url`.
    #[schema(value_type = Option<String>, format = Binary)]
    file: Option<Vec<u8>>,
    /// A link to download the file from, instead of sending it. It also
    /// becomes the source when none is given.
    url: Option<String>,
    /// `g`, `s`, `q` or `e`.
    rating: String,
    /// Whitespace-separated. A category prefix (`artist:name`) sets the
    /// category of a tag that's new.
    tags: Option<String>,
    source: Option<String>,
    description: Option<String>,
}

fn upload_error(error: UploadError) -> AppError {
    match error {
        UploadError::Invalid(message) => AppError::Unprocessable(message),
        UploadError::Duplicate(id) => AppError::Duplicate(id),
        UploadError::Internal(detail) => AppError::Internal(detail),
    }
}

/// Upload a post.
///
/// Needs `upload`. The post is `pending` when the site reviews uploads and
/// you lack `upload_without_approval`. Thumbnails are made in the
/// background: `file.processed` turns true when they're ready.
#[utoipa::path(
    post,
    path = "/posts",
    operation_id = "upload_post",
    tag = "posts",
    request_body(content = UploadRequest, content_type = "multipart/form-data"),
    responses(
        (status = 201, body = ApiPost, headers(("Location" = String, description = "The new post"))),
        (status = 409, body = ErrorBody, description = "The file was already uploaded; `post_id` names that post"),
        (status = 413, body = ErrorBody, description = "The request is larger than the site allows"),
        (status = 422, body = ErrorBody, description = "A field or the file isn't acceptable"),
    ),
)]
pub(crate) async fn upload(
    State(state): State<AppState>,
    current: CurrentUser,
    multipart: Multipart,
) -> Result<(StatusCode, [(HeaderName, String); 1], Json<ApiPost>), AppError> {
    current.require(Permission::Upload)?;
    let (mut fields, file) = crate::upload::receive(&state, multipart).await;
    let file = match file.map_err(upload_error)? {
        Some(file) => file,
        None if !fields.url.is_empty() => crate::upload::fetch_url(&state, &mut fields)
            .await
            .map_err(upload_error)?,
        None => {
            return Err(AppError::Unprocessable(
                "Send a `file`, or a `url` to download it from.".into(),
            ));
        }
    };
    let id = crate::upload::ingest(&state, &current, &file, &fields)
        .await
        .map_err(upload_error)?;
    let post = one(&state, &current, id).await?;
    Ok((
        StatusCode::CREATED,
        [(LOCATION, format!("{}/posts/{id}", super::BASE))],
        Json(post),
    ))
}

/// Changes to a post. Fields left out stay as they are.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PostChanges {
    /// `g`, `s`, `q` or `e`.
    rating: Option<String>,
    source: Option<String>,
    description: Option<String>,
    /// Another post's number, or `null` for none.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<i64>)]
    parent_id: Option<Option<i64>>,
    /// The complete new list of tags. A category prefix (`artist:name`)
    /// sets the category of a tag that's new. Can't be combined with
    /// `add_tags` or `remove_tags`.
    tags: Option<Vec<String>>,
    /// Tags to add, keeping the others.
    #[serde(default)]
    add_tags: Vec<String>,
    /// Tags to take off, keeping the others.
    #[serde(default)]
    remove_tags: Vec<String>,
}

/// Tells a field set to `null` (`Some(None)`) from one left out (`None`).
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Edit a post.
///
/// Needs `edit_posts`. Tag changes are applied to the post's tags as they
/// are when the change is saved, so edits made meanwhile by others are
/// kept. Every change is recorded in the post's history.
#[utoipa::path(
    patch,
    path = "/posts/{id}",
    operation_id = "update_post",
    tag = "posts",
    params(("id" = i64, Path, description = "Post number")),
    request_body = PostChanges,
    responses(
        (status = 200, body = ApiPost),
        (status = 404, body = ErrorBody),
        (status = 422, body = ErrorBody, description = "A change isn't acceptable"),
    ),
)]
pub(crate) async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(changes): Json<PostChanges>,
) -> Result<Json<ApiPost>, AppError> {
    current.require(Permission::EditPosts)?;
    let db = state.db.primary();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if !visibility(&current).allows(&post) {
        return Err(AppError::NotFound);
    }
    let names: Vec<String> = tags::by_ids(db, &post.tag_ids)
        .await?
        .into_iter()
        .map(|t| t.name)
        .collect();
    let lists = [&changes.add_tags, &changes.remove_tags];
    if lists
        .iter()
        .chain(changes.tags.as_ref().iter())
        .any(|list| {
            list.iter()
                .any(|tag| tag.trim().contains(char::is_whitespace))
        })
    {
        return Err(AppError::Unprocessable(
            "Tags can't contain spaces; use `_`.".into(),
        ));
    }
    let new_tags = match &changes.tags {
        Some(_) if lists.iter().any(|l| !l.is_empty()) => {
            return Err(AppError::Unprocessable(
                "Send either `tags`, or `add_tags` and `remove_tags`.".into(),
            ));
        }
        Some(all) => all.join(" "),
        None => {
            let removed: Vec<String> = changes
                .remove_tags
                .iter()
                .map(|t| uwu_core::tags::normalize(t))
                .collect();
            names
                .iter()
                .filter(|name| !removed.contains(name))
                .chain(changes.add_tags.iter())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        }
    };
    let form = EditForm {
        tags: new_tags,
        old_tags: names.join(" "),
        rating: changes
            .rating
            .unwrap_or_else(|| post.rating.code().to_owned()),
        source: changes.source.unwrap_or(post.source),
        description: changes.description.unwrap_or(post.description),
        parent: match changes.parent_id {
            None => post.parent_id.map(|p| p.to_string()).unwrap_or_default(),
            Some(parent) => parent.map(|p| p.to_string()).unwrap_or_default(),
        },
    };
    match crate::edit::apply(&state, &current, id, &form).await {
        Ok(()) => Ok(Json(one(&state, &current, id).await?)),
        Err(Refused::Invalid(message)) => Err(AppError::Unprocessable(message)),
        Err(Refused::Error(error)) => Err(error),
    }
}

/// Favorite a post.
///
/// Needs `favorite`. Favoriting twice is harmless.
#[utoipa::path(
    put,
    path = "/posts/{id}/favorite",
    operation_id = "favorite_post",
    tag = "posts",
    params(("id" = i64, Path, description = "Post number")),
    responses((status = 200, body = Reactions), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn favorite(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Reactions>, AppError> {
    let user = favorites::user_for(&state, &current, id, Permission::Favorite).await?;
    let db = state.db.primary();
    uwu_db::favorites::add(db, user, id).await?;
    Ok(Json(favorites::reactions(db, id, user).await?))
}

/// Unfavorite a post.
///
/// Needs `favorite`.
#[utoipa::path(
    delete,
    path = "/posts/{id}/favorite",
    operation_id = "unfavorite_post",
    tag = "posts",
    params(("id" = i64, Path, description = "Post number")),
    responses((status = 200, body = Reactions), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn unfavorite(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Reactions>, AppError> {
    let user = favorites::user_for(&state, &current, id, Permission::Favorite).await?;
    let db = state.db.primary();
    uwu_db::favorites::remove(db, user, id).await?;
    Ok(Json(favorites::reactions(db, id, user).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct Vote {
    /// 1 (up), -1 (down), or 0 to take your vote back.
    score: i16,
}

/// Vote on a post.
///
/// Needs `vote`. A new vote replaces your earlier one.
#[utoipa::path(
    put,
    path = "/posts/{id}/vote",
    operation_id = "vote_on_post",
    tag = "posts",
    params(("id" = i64, Path, description = "Post number")),
    request_body = Vote,
    responses(
        (status = 200, body = Reactions),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
    ),
)]
pub(crate) async fn vote(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(vote): Json<Vote>,
) -> Result<Json<Reactions>, AppError> {
    let user = favorites::user_for(&state, &current, id, Permission::Vote).await?;
    let db = state.db.primary();
    favorites::vote_on(db, id, user, vote.score).await?;
    Ok(Json(favorites::reactions(db, id, user).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewFlag {
    /// Why the post should be deleted.
    reason: String,
}

/// Flag a post for deletion.
///
/// Needs `flag`. Moderators review flags; a post can't have two open flags
/// from the same user.
#[utoipa::path(
    post,
    path = "/posts/{id}/flags",
    operation_id = "flag_post",
    tag = "posts",
    params(("id" = i64, Path, description = "Post number")),
    request_body = NewFlag,
    responses(
        (status = 204, description = "Flagged"),
        (status = 400, body = ErrorBody, description = "No reason, or the post can't be flagged now"),
    ),
)]
pub(crate) async fn flag(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(flag): Json<NewFlag>,
) -> Result<StatusCode, AppError> {
    crate::moderation::flag_post(&state, &current, id, &flag.reason).await?;
    Ok(StatusCode::NO_CONTENT)
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

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn uploads_through_the_api(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(40, 30);
        let fields = vec![
            ("rating", "q".to_owned()),
            ("tags", "cat artist:someone".to_owned()),
        ];
        let created = app
            .post_multipart(
                "/api/v1/posts",
                Some(&alice),
                &fields,
                Some(("a.png", &png)),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let post = json(&created.body);
        let id = post["id"].as_i64().unwrap();
        assert_eq!(created.location, Some(format!("/api/v1/posts/{id}")));
        assert_eq!(post["rating"], json!("q"));
        assert_eq!(
            post["tags"][1],
            json!({"name": "someone", "category": "artist"})
        );

        let again = app
            .post_multipart(
                "/api/v1/posts",
                Some(&alice),
                &fields,
                Some(("b.png", &png)),
            )
            .await;
        assert_eq!(again.status, StatusCode::CONFLICT);
        assert_eq!(json(&again.body)["error"]["post_id"], json!(id));

        let no_rating = vec![("tags", "cat".to_owned())];
        let response = app
            .post_multipart(
                "/api/v1/posts",
                Some(&alice),
                &no_rating,
                Some(("c.png", &fixture::png(42, 30))),
            )
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json(&response.body)["error"]["message"],
            json!("Choose a rating.")
        );
        let response = app
            .post_multipart("/api/v1/posts", Some(&alice), &fields, None)
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);

        let visitor = app
            .post_multipart("/api/v1/posts", None, &fields, Some(("d.png", &png)))
            .await;
        assert_eq!(visitor.status, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn edits_through_the_api(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let parent = upload(&app, &alice, &fixture::png(20, 20), "a").await;
        let id = upload(&app, &alice, &fixture::png(24, 20), "cat cute").await;
        let path = format!("/api/v1/posts/{id}");
        let tags = |post: &serde_json::Value| -> Vec<String> {
            post["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect()
        };

        let edited = app
            .json(
                "PATCH",
                &path,
                Some(&alice),
                Some(json!({"add_tags": ["dog", "artist:someone"], "remove_tags": ["Cute"], "rating": "e", "parent_id": parent})),
            )
            .await;
        assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
        let post = json(&edited.body);
        assert_eq!(tags(&post), ["cat", "dog", "someone"]);
        assert_eq!(
            (&post["rating"], &post["parent_id"]),
            (&json!("e"), &json!(parent))
        );

        // Left out stays; null clears.
        let post = json(
            &app.json(
                "PATCH",
                &path,
                Some(&alice),
                Some(json!({"source": "https://example.com"})),
            )
            .await
            .body,
        );
        assert_eq!(post["parent_id"], json!(parent));
        assert_eq!(post["rating"], json!("e"));
        let post = json(
            &app.json(
                "PATCH",
                &path,
                Some(&alice),
                Some(json!({"parent_id": null, "tags": ["bird"]})),
            )
            .await
            .body,
        );
        assert_eq!(post["parent_id"], json!(null));
        assert_eq!(tags(&post), ["bird"]);
        assert_eq!(post["source"], json!("https://example.com"));

        for (body, message) in [
            (json!({"tags": ["a"], "add_tags": ["b"]}), "either"),
            (json!({"add_tags": ["two words"]}), "spaces"),
            (json!({"rating": "x"}), "Choose a rating."),
            (json!({"parent_id": id}), "its own parent"),
            (json!({"colour": "red"}), "unknown field"),
        ] {
            let response = app.json("PATCH", &path, Some(&alice), Some(body)).await;
            assert_eq!(
                response.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{message}"
            );
            assert!(
                json(&response.body)["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(message),
                "{message}: {}",
                response.body
            );
        }
        let visitor = app
            .json("PATCH", &path, None, Some(json!({"rating": "g"})))
            .await;
        assert_eq!(visitor.status, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn reactions_and_flags(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, &fixture::png(20, 20), "cat").await;
        let path = |rest: &str| format!("/api/v1/posts/{id}/{rest}");

        let faved = json(
            &app.json("PUT", &path("favorite"), Some(&alice), None)
                .await
                .body,
        );
        assert_eq!(
            faved,
            json!({"fav_count": 1, "favorited": true, "score": 0, "vote": 0})
        );
        let voted = json(
            &app.json(
                "PUT",
                &path("vote"),
                Some(&alice),
                Some(json!({"score": -1})),
            )
            .await
            .body,
        );
        assert_eq!((&voted["score"], &voted["vote"]), (&json!(-1), &json!(-1)));
        let bad_vote = app
            .json(
                "PUT",
                &path("vote"),
                Some(&alice),
                Some(json!({"score": 5})),
            )
            .await;
        assert_eq!(bad_vote.status, StatusCode::BAD_REQUEST);
        let unfaved = json(
            &app.json("DELETE", &path("favorite"), Some(&alice), None)
                .await
                .body,
        );
        assert_eq!(unfaved["favorited"], json!(false));

        let flagged = app
            .json(
                "POST",
                &path("flags"),
                Some(&alice),
                Some(json!({"reason": "blurry"})),
            )
            .await;
        assert_eq!(flagged.status, StatusCode::NO_CONTENT, "{}", flagged.body);
        let again = app
            .json(
                "POST",
                &path("flags"),
                Some(&alice),
                Some(json!({"reason": "blurry"})),
            )
            .await;
        assert_eq!(again.status, StatusCode::BAD_REQUEST);
        let post = json(&app.get(&format!("/api/v1/posts/{id}"), None).await.body);
        assert_eq!(post["status"], json!("flagged"));
    }
}
