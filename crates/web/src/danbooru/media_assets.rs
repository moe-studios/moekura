//! `/media_assets.json`, `/media_assets/{id}.json` and
//! `/media_metadata.json`. A post has one file, whose id is the post's.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::search::PageRef;
use serde::{Deserialize, Serialize};

use super::{ListParams, json};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/media_assets", get(index))
        .route("/media_assets/{id}", get(show))
        .route("/media_metadata", get(metadata))
}

#[derive(Debug, Default, Deserialize)]
struct AssetParams {
    #[serde(rename = "search[id]", default)]
    id: String,
    #[serde(rename = "search[md5]", default)]
    md5: String,
    #[serde(flatten)]
    list: ListParams,
}

/// The search for the posts whose files these parameters ask for.
fn search(id: &str, md5: &str) -> String {
    let mut terms = Vec::new();
    if !id.trim().is_empty() {
        terms.push(format!("id:{}", id.trim()));
    }
    if !md5.trim().is_empty() {
        terms.push(format!("md5:{}", md5.trim()));
    }
    terms.join(" ")
}

async fn assets(
    state: &AppState,
    current: &CurrentUser,
    tags: &str,
    list: &ListParams,
) -> Result<Vec<super::posts::MediaAsset>, AppError> {
    let page: PageRef = match list.page.trim() {
        "" => PageRef::default(),
        page => page
            .parse()
            .map_err(|()| AppError::BadRequest("`page` must be a number".into()))?,
    };
    let limit = list.limit(state.search_config().max_per_page);
    let found = super::posts::find(state, current, tags, limit, page).await?;
    let db = state.reader(current);
    Ok(super::posts::danbooru_posts(state, db, found)
        .await?
        .into_iter()
        .map(|p| p.media_asset)
        .collect())
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<AssetParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let tags = search(&params.id, &params.md5);
    json(
        assets(&state, &current, &tags, &params.list).await?,
        &params.list.only,
    )
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let asset = assets(
        &state,
        &current,
        &format!("id:{id}"),
        &ListParams::default(),
    )
    .await?
    .pop()
    .ok_or(AppError::NotFound)?;
    json(asset, "")
}

#[derive(Debug, Serialize)]
struct MediaMetadata {
    id: i64,
    media_asset_id: i64,
    metadata: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct MetadataParams {
    #[serde(rename = "search[media_asset_id]", default)]
    media_asset_id: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn metadata(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<MetadataParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let tags = search(&params.media_asset_id, "");
    let found = assets(&state, &current, &tags, &params.list).await?;
    let db = state.reader(&current);
    let mut list = Vec::with_capacity(found.len());
    for asset in found {
        let metadata = moekura_db::media::metadata_for_post(db, asset.id)
            .await?
            .unwrap_or_default();
        list.push(MediaMetadata {
            id: asset.id,
            media_asset_id: asset.id,
            metadata,
        });
    }
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
    async fn assets_and_metadata(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let first = upload(&app, &alice, 20, "cat").await;
        let second = upload(&app, &alice, 24, "cat").await;
        sqlx::query(
            "UPDATE media_assets SET metadata = '{\"EXIF:Make\": \"Canon\"}' WHERE post_id = $1",
        )
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
        let parse = |body: &str| serde_json::from_str::<Value>(body).unwrap();
        let all = parse(&app.get("/media_assets.json", None).await.body);
        let ids: Vec<i64> = all
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, [second, first]);
        let md5 = all[1]["md5"].as_str().unwrap().to_owned();
        let by_md5 = parse(
            &app.get(&format!("/media_assets.json?search[md5]={md5}"), None)
                .await
                .body,
        );
        assert_eq!(by_md5[0]["id"].as_i64(), Some(first));
        let one = parse(
            &app.get(&format!("/media_assets/{first}.json"), None)
                .await
                .body,
        );
        assert_eq!(one["image_width"], 20);
        let metadata = parse(
            &app.get(
                &format!("/media_metadata.json?search[media_asset_id]={first}"),
                None,
            )
            .await
            .body,
        );
        assert_eq!(metadata[0]["metadata"]["EXIF:Make"], "Canon");
    }
}
