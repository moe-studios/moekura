//! `/wiki_page_versions.json` and `/pool_versions.json`, as Danbooru
//! describes them (tag, artist and note versions live beside their
//! things).

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_core::pools::post_changes;
use moekura_db::{pools, wiki};
use serde::{Deserialize, Serialize};

use super::community::{MAX_LISTED, check_listed};
use super::tags::window;
use super::{ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/wiki_page_versions", get(wiki_versions))
        .route("/pool_versions", get(pool_versions))
}

/// A user by id or name; something that doesn't parse or exist matches
/// nothing.
async fn updater(db: &sqlx::PgPool, id: &str, name: &str) -> Result<Option<i64>, AppError> {
    Ok(match (id.trim(), name.trim()) {
        ("", "") => None,
        ("", name) => Some(
            moekura_db::users::by_name(db, name)
                .await?
                .map_or(-1, |u| u.id),
        ),
        (id, _) => Some(id.parse().unwrap_or(-1)),
    })
}

#[derive(Debug, Serialize)]
struct DanbooruWikiVersion {
    id: i64,
    wiki_page_id: i32,
    title: String,
    body: String,
    other_names: Vec<String>,
    updater_id: Option<i64>,
    is_locked: bool,
    is_deleted: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Default, Deserialize)]
struct WikiParams {
    #[serde(rename = "search[wiki_page_id]", default)]
    wiki_page_id: String,
    #[serde(rename = "search[title]", default)]
    title: String,
    #[serde(rename = "search[updater_id]", default)]
    updater_id: String,
    #[serde(rename = "search[updater_name]", default)]
    updater_name: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn wiki_versions(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<WikiParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let (offset, limit) = window(&params.list, 1000)?;
    let page_id = match (params.wiki_page_id.trim(), params.title.trim()) {
        ("", "") => None,
        ("", title) => Some(
            wiki::by_title(db, &moekura_core::tags::normalize(title))
                .await?
                .map_or(-1, |p| p.id),
        ),
        (id, _) => Some(id.parse().unwrap_or(-1)),
    };
    let updater_id = updater(db, &params.updater_id, &params.updater_name).await?;
    let list: Vec<DanbooruWikiVersion> =
        wiki::full_versions(db, page_id, updater_id, offset, limit)
            .await?
            .into_iter()
            .map(|v| {
                let at = timestamp(v.created_at);
                DanbooruWikiVersion {
                    id: v.id,
                    wiki_page_id: v.wiki_page_id,
                    title: v.title,
                    body: v.body,
                    other_names: v.other_names,
                    updater_id: v.updater_id,
                    is_locked: false,
                    is_deleted: false,
                    updated_at: at.clone(),
                    created_at: at,
                }
            })
            .collect();
    json(list, &params.list.only)
}

#[derive(Debug, Serialize)]
struct DanbooruPoolVersion {
    id: i64,
    pool_id: i32,
    version: i32,
    post_ids: Vec<i64>,
    added_post_ids: Vec<i64>,
    removed_post_ids: Vec<i64>,
    updater_id: Option<i64>,
    name: String,
    name_changed: bool,
    description: String,
    description_changed: bool,
    category: String,
    is_active: bool,
    is_deleted: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Default, Deserialize)]
struct PoolParams {
    #[serde(rename = "search[pool_id]", default)]
    pool_id: String,
    #[serde(rename = "search[updater_id]", default)]
    updater_id: String,
    #[serde(rename = "search[updater_name]", default)]
    updater_name: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn pool_versions(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<PoolParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    // Each version lists every post the pool had then.
    let (offset, limit) = window(&params.list, MAX_LISTED)?;
    let pool_id = match params.pool_id.trim() {
        "" => None,
        id => Some(id.parse().unwrap_or(-1)),
    };
    let updater_id = updater(db, &params.updater_id, &params.updater_name).await?;
    let deleted = crate::pools::sees_deleted(&current);
    let list: Vec<DanbooruPoolVersion> =
        pools::full_versions(db, pool_id, updater_id, deleted, offset, limit)
            .await?
            .into_iter()
            .map(|v| {
                let at = timestamp(v.created_at);
                let (added_post_ids, removed_post_ids) = post_changes(
                    v.previous_post_ids.as_deref().unwrap_or_default(),
                    &v.post_ids,
                );
                let first = v.previous_name.is_none();
                DanbooruPoolVersion {
                    id: v.id,
                    pool_id: v.pool_id,
                    version: v.version,
                    added_post_ids,
                    removed_post_ids,
                    name_changed: first || v.previous_name.as_deref() != Some(v.name.as_str()),
                    description_changed: first
                        || v.previous_description.as_deref() != Some(v.description.as_str()),
                    post_ids: v.post_ids,
                    updater_id: v.updater_id,
                    name: v.name,
                    description: v.description,
                    category: v.category,
                    is_active: !v.is_deleted,
                    is_deleted: v.is_deleted,
                    updated_at: at.clone(),
                    created_at: at,
                }
            })
            .collect();
    check_listed(
        "pool versions",
        list.iter()
            .map(|v| v.post_ids.len() + v.added_post_ids.len() + v.removed_post_ids.len())
            .sum(),
    )?;
    json(list, &params.list.only)
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn wiki_and_pool_versions(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let user = moekura_db::users::by_name(&pool, "alice")
            .await
            .unwrap()
            .unwrap();
        moekura_db::wiki::save(&pool, "cat", "A cat.", Some(user.id), None)
            .await
            .unwrap();
        moekura_db::wiki::save(&pool, "cat", "A small cat.", Some(user.id), None)
            .await
            .unwrap();
        let parse = |body: &str| serde_json::from_str::<Value>(body).unwrap();
        let wiki = parse(
            &app.get("/wiki_page_versions.json?search[title]=cat", None)
                .await
                .body,
        );
        assert_eq!(wiki.as_array().unwrap().len(), 2);
        assert_eq!(wiki[0]["body"], "A small cat.");
        assert_eq!(wiki[0]["updater_id"].as_i64(), Some(user.id));

        let a = upload(&app, &alice, 20, "cat").await;
        let b = upload(&app, &alice, 24, "cat").await;
        let contents = |posts: Vec<i64>| moekura_db::pools::Contents {
            name: "comic".into(),
            description: String::new(),
            category: "series".into(),
            is_deleted: false,
            post_ids: posts,
        };
        let id = moekura_db::pools::create(&pool, &contents(vec![a]), Some(user.id))
            .await
            .unwrap();
        moekura_db::pools::save(&pool, id, &contents(vec![b]), Some(user.id), None)
            .await
            .unwrap();
        let versions = parse(
            &app.get(&format!("/pool_versions.json?search[pool_id]={id}"), None)
                .await
                .body,
        );
        assert_eq!(versions[0]["added_post_ids"][0].as_i64(), Some(b));
        assert_eq!(versions[0]["removed_post_ids"][0].as_i64(), Some(a));
        assert_eq!(versions[0]["name_changed"], false);
        assert_eq!(versions[1]["name_changed"], true);
    }

    /// A pool with `versions` versions listing posts 1 to `posts` (versions
    /// keep their post ids as a list, posts or not).
    async fn pool_with(pool: &PgPool, name: &str, versions: i32, posts: i64) -> i32 {
        let id: i32 = sqlx::query_scalar("INSERT INTO pools (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO pool_versions
                 (pool_id, version, name, description, category, is_deleted, post_ids)
             SELECT $1, v, $2, '', 'series', false, ARRAY(SELECT generate_series(1::bigint, $4))
             FROM generate_series(1, $3) v",
        )
        .bind(id)
        .bind(name)
        .bind(versions)
        .bind(posts)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn pool_version_pages_stay_small(pool: PgPool) {
        let app = app(&pool).await;
        let small = pool_with(&pool, "small", 110, 10).await;
        let big = pool_with(&pool, "big", 11, 10_000).await;
        let path =
            |id: i32, limit: u32| format!("/pool_versions.json?search[pool_id]={id}&limit={limit}");
        let parse = |body: &str| serde_json::from_str::<Value>(body).unwrap();

        // At most 100 a page.
        let listed = parse(&app.get(&path(small, 1000), None).await.body);
        assert_eq!(listed.as_array().map(Vec::len), Some(100));
        // And no more than 100,000 post ids between them.
        let refused = app.get(&path(big, 11), None).await;
        assert_eq!(
            refused.status,
            axum::http::StatusCode::BAD_REQUEST,
            "{}",
            refused.body
        );
        assert!(refused.body.contains("ask for fewer"), "{}", refused.body);
        let listed = parse(&app.get(&path(big, 5), None).await.body);
        assert_eq!(listed[0]["post_ids"].as_array().map(Vec::len), Some(10_000));
    }
}
