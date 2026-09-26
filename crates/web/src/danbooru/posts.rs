//! `/posts.json`, `/posts/{id}.json`, `/posts/random.json` and
//! `/counts/posts.json`.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_core::search::Query as SearchQuery;
use moekura_db::posts::{self, Extras, Post};
use moekura_db::search::{PageRef, Plan, SearchError};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use super::{Fields, ListParams, json, timestamp};
use crate::AppState;
use crate::api::posts::{ApiPost, load, one};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::posts::visibility;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/posts", get(index).post(super::uploads::create_post))
        .route("/posts/random", get(random))
        .route("/posts/{id}", get(show).put(update).patch(update))
        .route("/counts/posts", get(count))
        .route("/post_versions", get(versions))
}

/// A post as Danbooru describes it.
#[derive(Debug, Serialize)]
pub(crate) struct DanbooruPost {
    id: i64,
    created_at: String,
    updated_at: String,
    uploader_id: Option<i64>,
    approver_id: Option<i64>,
    score: i32,
    up_score: i64,
    down_score: i64,
    fav_count: i32,
    source: String,
    md5: String,
    rating: String,
    image_width: i32,
    image_height: i32,
    tag_string: String,
    tag_string_general: String,
    tag_string_artist: String,
    tag_string_copyright: String,
    tag_string_character: String,
    tag_string_meta: String,
    tag_count: usize,
    tag_count_general: usize,
    tag_count_artist: usize,
    tag_count_copyright: usize,
    tag_count_character: usize,
    tag_count_meta: usize,
    file_ext: String,
    file_size: i64,
    file_url: String,
    large_file_url: String,
    preview_file_url: String,
    parent_id: Option<i64>,
    has_children: bool,
    has_active_children: bool,
    has_visible_children: bool,
    has_large: bool,
    is_pending: bool,
    is_flagged: bool,
    is_deleted: bool,
    is_banned: bool,
    pixiv_id: Option<i64>,
    bit_flags: i32,
    last_comment_bumped_at: Option<String>,
    last_commented_at: Option<String>,
    last_noted_at: Option<String>,
    media_asset: MediaAsset,
}

#[derive(Debug, Serialize)]
struct MediaAsset {
    id: i64,
    created_at: String,
    updated_at: String,
    md5: String,
    file_ext: String,
    file_size: i64,
    image_width: i32,
    image_height: i32,
    /// Seconds, for videos and animations.
    duration: Option<f64>,
    status: &'static str,
    is_public: bool,
    variants: Vec<MediaVariant>,
}

#[derive(Debug, Serialize)]
struct MediaVariant {
    #[serde(rename = "type")]
    kind: String,
    url: String,
    width: i32,
    height: i32,
    file_ext: String,
}

/// Danbooru's extensions: `jpg` rather than `jpeg`.
fn extension(format: &str) -> String {
    match format {
        "jpeg" => "jpg".to_owned(),
        other => other.to_owned(),
    }
}

impl DanbooruPost {
    fn new(post: ApiPost, uploader_id: Option<i64>, extras: Option<&Extras>) -> Self {
        let named = |category: &str| -> Vec<&str> {
            post.tags
                .iter()
                .filter(|t| t.category == category)
                .map(|t| t.name.as_str())
                .collect()
        };
        let [general, artist, copyright, character, meta] =
            ["general", "artist", "copyright", "character", "meta"].map(named);
        let tag_string = post
            .tags
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let file = &post.file;
        let file_ext = extension(&file.media_type);
        let is_video = matches!(file.media_type.as_str(), "mp4" | "webm");

        // Moekura's thumbnails, smallest first, stand in for Danbooru's
        // fixed sizes.
        let mut thumbs: Vec<_> = post
            .variants
            .iter()
            .filter(|v| v.kind.starts_with("thumb-"))
            .collect();
        thumbs.sort_by_key(|v| v.width.max(v.height));
        let sample = post
            .variants
            .iter()
            .find(|v| v.kind == "sample")
            .filter(|_| !is_video);
        let mut variants: Vec<MediaVariant> = thumbs
            .iter()
            .zip(["180x180", "360x360", "720x720"])
            .map(|(v, kind)| MediaVariant {
                kind: kind.to_owned(),
                url: v.url.clone(),
                width: v.width,
                height: v.height,
                file_ext: extension(&v.format),
            })
            .collect();
        if let Some(v) = sample {
            variants.push(MediaVariant {
                kind: "sample".to_owned(),
                url: v.url.clone(),
                width: v.width,
                height: v.height,
                file_ext: extension(&v.format),
            });
        }
        variants.push(MediaVariant {
            kind: "original".to_owned(),
            url: file.url.clone(),
            width: file.width,
            height: file.height,
            file_ext: file_ext.clone(),
        });

        let created = timestamp(post.created_at);
        let md5 = file.md5.clone();
        let extras = extras.cloned().unwrap_or_default();
        Self {
            id: post.id,
            updated_at: created.clone(),
            uploader_id,
            approver_id: None,
            score: post.score,
            up_score: extras.up_votes,
            down_score: -extras.down_votes,
            fav_count: post.fav_count,
            rating: post.rating.clone(),
            image_width: file.width,
            image_height: file.height,
            tag_count: post.tags.len(),
            tag_count_general: general.len(),
            tag_count_artist: artist.len(),
            tag_count_copyright: copyright.len(),
            tag_count_character: character.len(),
            tag_count_meta: meta.len(),
            tag_string,
            tag_string_general: general.join(" "),
            tag_string_artist: artist.join(" "),
            tag_string_copyright: copyright.join(" "),
            tag_string_character: character.join(" "),
            tag_string_meta: meta.join(" "),
            file_size: file.size,
            file_url: file.url.clone(),
            large_file_url: sample.map_or_else(|| file.url.clone(), |v| v.url.clone()),
            preview_file_url: thumbs.first().map_or_else(String::new, |v| v.url.clone()),
            has_large: sample.is_some(),
            parent_id: post.parent_id,
            has_children: extras.has_children,
            has_active_children: extras.has_active_children,
            has_visible_children: extras.has_active_children,
            is_pending: post.status == "pending",
            is_flagged: post.status == "flagged",
            is_deleted: post.status == "deleted",
            is_banned: false,
            pixiv_id: None,
            bit_flags: 0,
            last_comment_bumped_at: post.last_commented_at.map(timestamp),
            last_commented_at: post.last_commented_at.map(timestamp),
            last_noted_at: post.last_noted_at.map(timestamp),
            media_asset: MediaAsset {
                id: post.id,
                created_at: created.clone(),
                updated_at: created.clone(),
                md5: md5.clone(),
                file_ext: file_ext.clone(),
                file_size: file.size,
                image_width: file.width,
                image_height: file.height,
                duration: file.duration_ms.map(|ms| f64::from(ms) / 1000.0),
                status: if file.processed {
                    "active"
                } else {
                    "processing"
                },
                is_public: true,
                variants,
            },
            created_at: created,
            source: post.source,
            md5,
            file_ext,
        }
    }
}

/// `posts` in Danbooru's shape, in the same order.
pub(crate) async fn danbooru_posts(
    state: &AppState,
    db: &PgPool,
    posts: Vec<Post>,
) -> sqlx::Result<Vec<DanbooruPost>> {
    let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();
    let uploaders: HashMap<i64, Option<i64>> =
        posts.iter().map(|p| (p.id, p.uploader_id)).collect();
    let extras: HashMap<i64, Extras> = posts::extras(db, &ids)
        .await?
        .into_iter()
        .map(|e| (e.post_id, e))
        .collect();
    Ok(load(state, db, posts)
        .await?
        .into_iter()
        .map(|post| {
            let uploader = uploaders.get(&post.id).copied().flatten();
            let extra = extras.get(&post.id);
            DanbooruPost::new(post, uploader, extra)
        })
        .collect())
}

pub(super) fn search_error(error: SearchError) -> AppError {
    match error {
        SearchError::Invalid(message) => AppError::Unprocessable(message),
        SearchError::Db(error) => error.into(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct SearchParams {
    #[serde(default)]
    tags: String,
    #[serde(flatten)]
    list: ListParams,
}

/// The posts matching `tags`, on `page`, at most `limit`.
pub(super) async fn find(
    state: &AppState,
    current: &CurrentUser,
    tags: &str,
    limit: u32,
    page: PageRef,
) -> Result<Vec<Post>, AppError> {
    let db = state.reader(current);
    let mut query =
        SearchQuery::parse(tags.trim()).map_err(|e| AppError::Unprocessable(e.to_string()))?;
    query.limit = Some(limit);
    let plan = Plan::resolve(db, &query, &visibility(current), &state.config.search)
        .await
        .map_err(search_error)?;
    let ids = plan.ids(db, page).await.map_err(search_error)?;
    let mut found = posts::by_ids(db, &ids).await?;
    found.sort_by_key(|p| ids.iter().position(|id| *id == p.id));
    Ok(found)
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<SearchParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let list = &params.list;
    let page: PageRef = match list.page.trim() {
        "" => PageRef::default(),
        page => page
            .parse()
            .map_err(|()| AppError::BadRequest("`page` must be a number, b<id> or a<id>".into()))?,
    };
    let limit = list.limit(state.config.search.max_per_page);
    let found = find(&state, &current, &params.tags, limit, page).await?;
    let db = state.reader(&current);
    json(danbooru_posts(&state, db, found).await?, &list.only)
}

#[derive(Debug, Default, Deserialize)]
struct ShowParams {
    #[serde(default)]
    only: String,
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(params): Query<ShowParams>,
) -> Result<Response, AppError> {
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    one(&state, &current, id).await?;
    let db = state.db.primary();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    let mut found = danbooru_posts(&state, db, vec![post]).await?;
    json(found.pop().ok_or(AppError::NotFound)?, &params.only)
}

async fn random(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<SearchParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let tags = format!("{} order:random", params.tags);
    let found = find(&state, &current, &tags, 1, PageRef::default()).await?;
    if found.is_empty() {
        return Err(AppError::NotFound);
    }
    let db = state.reader(&current);
    let mut posts = danbooru_posts(&state, db, found).await?;
    json(posts.pop().ok_or(AppError::NotFound)?, &params.list.only)
}

/// Changes a post as Danbooru clients send it: `post[tag_string]`
/// (with `post[old_tag_string]`, the tags the client started from, so
/// others' changes meanwhile are kept), `post[rating]`, `post[source]`
/// and `post[parent_id]`. The same rules apply as on the site.
async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    fields: Fields,
) -> Result<Response, AppError> {
    current.require(Permission::EditPosts)?;
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let db = state.db.primary();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if !visibility(&current).allows(&post) {
        return Err(AppError::NotFound);
    }
    let names: Vec<String> = moekura_db::tags::by_ids(db, &post.tag_ids)
        .await?
        .into_iter()
        .map(|t| t.name)
        .collect();
    let current_tags = names.join(" ");
    let field = |name: &str| fields.get(&format!("post[{name}]")).map(str::to_owned);
    let form = crate::edit::EditForm {
        tags: field("tag_string").unwrap_or_else(|| current_tags.clone()),
        old_tags: field("old_tag_string").unwrap_or(current_tags),
        rating: field("rating").unwrap_or_else(|| post.rating.code().to_owned()),
        source: field("source").unwrap_or(post.source),
        description: field("description").unwrap_or(post.description),
        parent: field("parent_id")
            .unwrap_or_else(|| post.parent_id.map(|p| p.to_string()).unwrap_or_default()),
    };
    match crate::edit::apply(&state, &current, id, &form).await {
        Ok(()) => {}
        Err(crate::edit::Refused::Invalid(message)) => {
            return Err(AppError::Unprocessable(message));
        }
        Err(crate::edit::Refused::Error(error)) => return Err(error),
    }
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    let mut updated = danbooru_posts(&state, db, vec![post]).await?;
    json(updated.pop().ok_or(AppError::NotFound)?, "")
}

#[derive(Debug, Default, Deserialize)]
struct VersionParams {
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(flatten)]
    list: ListParams,
}

/// A post's history, newest first. Only by post (`search[post_id]`).
async fn versions(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<VersionParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let Ok(id) = params.post_id.trim().parse::<i64>() else {
        return json(Vec::<serde_json::Value>::new(), "");
    };
    one(&state, &current, id).await?;
    let db = state.reader(&current);
    let versions = moekura_db::post_versions::list(db, id).await?;
    let mut tag_ids: Vec<i32> = versions
        .iter()
        .flat_map(|v| v.tag_ids.iter().copied())
        .collect();
    tag_ids.sort_unstable();
    tag_ids.dedup();
    let names: HashMap<i32, String> = moekura_db::tags::by_ids(db, &tag_ids)
        .await?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect();
    let named = |ids: &[i32]| -> Vec<String> {
        let mut list: Vec<String> = ids.iter().filter_map(|id| names.get(id).cloned()).collect();
        list.sort();
        list
    };
    let limit = params.list.limit(1000) as usize;
    let result: Vec<serde_json::Value> = versions
        .iter()
        .enumerate()
        .take(limit)
        .map(|(i, v)| {
            let previous = versions.get(i + 1);
            let changed = |f: fn(&moekura_db::post_versions::Version) -> String| {
                previous.is_some_and(|p| f(p) != f(v))
            };
            serde_json::json!({
                "id": v.id,
                "post_id": id,
                "version": v.version,
                "tags": named(&v.tag_ids).join(" "),
                "added_tags": named(&v.added_tag_ids),
                "removed_tags": named(&v.removed_tag_ids),
                "rating": v.rating,
                "rating_changed": changed(|v| v.rating.clone()),
                "source": v.source,
                "source_changed": changed(|v| v.source.clone()),
                "parent_id": v.parent_id,
                "parent_changed": changed(|v| format!("{:?}", v.parent_id)),
                "updater_id": v.updater_id,
                "updated_at": timestamp(v.created_at),
                "obsolete_added_tags": "",
                "obsolete_removed_tags": "",
                "unchanged_tags": "",
            })
        })
        .collect();
    json(result, &params.list.only)
}

#[derive(Debug, Default, Deserialize)]
struct CountParams {
    #[serde(default)]
    tags: String,
}

async fn count(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<CountParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let query = SearchQuery::parse(params.tags.trim())
        .map_err(|e| AppError::Unprocessable(e.to_string()))?;
    let plan = Plan::resolve(db, &query, &visibility(&current), &state.config.search)
        .await
        .map_err(search_error)?;
    let count = state
        .counts
        .count(&plan, db, &current)
        .await
        .map_err(search_error)?;
    let value = match count {
        moekura_db::search::Count::Exact(n)
        | moekura_db::search::Count::About(n)
        | moekura_db::search::Count::AtLeast(n) => n,
    };
    Ok(axum::Json(serde_json::json!({ "counts": { "posts": value } })).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    fn body(response: &crate::test_support::TestResponse) -> Value {
        serde_json::from_str(&response.body).unwrap_or_else(|_| panic!("{}", response.body))
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn posts_in_danbooru_shape(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let first = upload(&app, &alice, 20, "cat artist:someone").await;
        let second = upload(&app, &alice, 22, "cat dog").await;

        let list = body(&app.get("/posts.json?tags=cat&limit=5", None).await);
        let ids: Vec<i64> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, [second, first]);
        let post = &list[1];
        assert_eq!(post["tag_string"], json!("cat someone"));
        assert_eq!(post["tag_string_artist"], json!("someone"));
        assert_eq!(post["tag_string_general"], json!("cat"));
        assert_eq!(post["tag_count_general"], json!(1));
        assert_eq!(post["rating"], json!("s"));
        assert_eq!(post["file_ext"], json!("png"));
        assert_eq!(post["image_width"], json!(20));
        assert!(
            post["file_url"]
                .as_str()
                .unwrap()
                .starts_with("http://localhost:8080/")
        );
        assert_eq!(post["is_deleted"], json!(false));
        assert!(
            post["media_asset"]["variants"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["type"] == "original")
        );
        assert!(post["created_at"].as_str().unwrap().contains('T'));

        // Paging as gallery-dl does it, and trimming fields.
        let before = body(
            &app.get(
                &format!("/posts.json?tags=cat&page=b{second}&only=id,md5"),
                None,
            )
            .await,
        );
        assert_eq!(before, json!([{ "id": first, "md5": list[1]["md5"] }]));
        let by_id = body(
            &app.get(&format!("/posts.json?tags=id:{first},{second}"), None)
                .await,
        );
        assert_eq!(by_id.as_array().unwrap().len(), 2);

        let one = body(&app.get(&format!("/posts/{first}.json"), None).await);
        assert_eq!(one["id"], json!(first));
        let random = body(&app.get("/posts/random.json?tags=dog", None).await);
        assert_eq!(random["id"], json!(second));
        let counts = body(&app.get("/counts/posts.json?tags=cat", None).await);
        assert_eq!(counts, json!({ "counts": { "posts": 2 } }));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn editing_and_history(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, 20, "cat cute").await;
        let path = format!("/posts/{id}.json");

        let visitor = app.form("PUT", &path, None, "post%5Brating%5D=e").await;
        assert_eq!(visitor.status, StatusCode::UNAUTHORIZED);
        // A JSON body, the way Boorusama sends it.
        let changes = json!({ "post": { "tag_string": "cat dog", "rating": "q" } });
        let edited = app.json("PUT", &path, Some(&alice), Some(changes)).await;
        assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
        let edited = body(&edited);
        assert_eq!(edited["tag_string"], json!("cat dog"));
        assert_eq!(edited["rating"], json!("q"));

        // A form body with the tags the client started from: someone else's
        // change in between stays.
        let form = "post%5Bold_tag_string%5D=cat+cute&post%5Btag_string%5D=cat+cute+whiskers";
        let response = app.form("PATCH", &path, Some(&alice), form).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert_eq!(body(&response)["tag_string"], json!("cat dog whiskers"));

        let bad = app
            .json(
                "PUT",
                &path,
                Some(&alice),
                Some(json!({ "post": { "rating": "x" } })),
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

        let versions = body(
            &app.get(&format!("/post_versions.json?search[post_id]={id}"), None)
                .await,
        );
        let versions = versions.as_array().unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0]["added_tags"], json!(["whiskers"]));
        assert_eq!(versions[1]["added_tags"], json!(["dog"]));
        assert_eq!(versions[1]["removed_tags"], json!(["cute"]));
        assert_eq!(versions[1]["rating_changed"], json!(true));
        assert_eq!(versions[2]["tags"], json!("cat cute"));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn errors_in_danbooru_shape(pool: PgPool) {
        let app = app(&pool).await;
        let missing = app.get("/posts/999.json", None).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert_eq!(body(&missing)["success"], json!(false));
        assert_eq!(body(&missing)["message"], json!("Not found"));
        let bad = app.get("/posts.json?page=x", None).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert!(body(&bad)["message"].as_str().unwrap().contains("page"));
        let empty = app.get("/posts/random.json?tags=nothing_here", None).await;
        assert_eq!(empty.status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn authenticates_with_api_keys(pool: PgPool) {
        // Pending posts are only visible to their uploader.
        moekura_db::settings::set(&pool, "upload_approval", json!(true))
            .await
            .unwrap();
        let app = app(&pool).await;
        let alice_session = session_for(&pool, "alice", SystemRole::Member).await;
        let user = moekura_db::users::by_name(&pool, "alice")
            .await
            .unwrap()
            .unwrap();
        let key = moekura_db::api_keys::create(&pool, user.id, "gallery-dl", None)
            .await
            .unwrap();
        session_for(&pool, "bob", SystemRole::Member).await;
        let id = upload(&app, &alice_session, 20, "secret").await;
        let path = format!("/posts/{id}.json");

        assert_eq!(app.get(&path, None).await.status, StatusCode::NOT_FOUND);
        let basic = format!("Basic {}", STANDARD.encode(format!("alice:{key}")));
        let with_basic = app
            .get_with_headers(&path, &[("authorization", &basic)])
            .await;
        assert_eq!(with_basic.status, StatusCode::OK, "{}", with_basic.body);
        let with_params = app
            .get(&format!("{path}?login=Alice&api_key={key}"), None)
            .await;
        assert_eq!(with_params.status, StatusCode::OK);

        // The key has to be that user's, and valid.
        let wrong_name = app
            .get(&format!("{path}?login=bob&api_key={key}"), None)
            .await;
        assert_eq!(wrong_name.status, StatusCode::UNAUTHORIZED);
        assert_eq!(body(&wrong_name)["success"], json!(false));
        let wrong_key = app
            .get(&format!("{path}?login=alice&api_key=mka_nope"), None)
            .await;
        assert_eq!(wrong_key.status, StatusCode::UNAUTHORIZED);
        // Basic auth elsewhere (a proxy's) is left alone.
        let page = app
            .get_with_headers("/posts", &[("authorization", &basic)])
            .await;
        assert_eq!(page.status, StatusCode::OK);
    }
}
