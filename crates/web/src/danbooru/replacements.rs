//! `/post_replacements.json`: replaced post files, read-only.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::replacements::{self, Filter};
use serde::{Deserialize, Serialize};

use super::tags::window;
use super::{ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/post_replacements", get(index))
}

#[derive(Debug, Serialize)]
struct DanbooruReplacement {
    id: i64,
    post_id: i64,
    creator_id: Option<i64>,
    original_url: String,
    replacement_url: String,
    md5_was: String,
    md5: String,
    file_ext_was: String,
    file_ext: String,
    image_width_was: i32,
    image_height_was: i32,
    image_width: i32,
    image_height: i32,
    file_size_was: i64,
    file_size: i64,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Default, Deserialize)]
struct Params {
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(rename = "search[creator_id]", default)]
    creator_id: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<Params>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let (offset, limit) = window(&params.list, 1000)?;
    let number = |v: &str| match v.trim() {
        "" => None,
        v => Some(v.parse().unwrap_or(-1)),
    };
    let filter = Filter {
        post_id: number(&params.post_id),
        creator_id: number(&params.creator_id),
    };
    let found = replacements::list(db, filter, offset, limit).await?;
    // Only for posts the viewer may see.
    let ids: Vec<i64> = found.iter().map(|r| r.post_id).collect();
    let posts = moekura_db::posts::by_ids(db, &ids).await?;
    let visible = crate::posts::visibility(&current);
    let ext = |t: &str| match t {
        "jpeg" => "jpg".to_owned(),
        "ugoira" => "zip".to_owned(),
        other => other.to_owned(),
    };
    let list: Vec<DanbooruReplacement> = found
        .into_iter()
        .filter(|r| posts.iter().any(|p| p.id == r.post_id && visible.allows(p)))
        .map(|r| {
            let at = timestamp(r.created_at);
            DanbooruReplacement {
                id: r.id,
                post_id: r.post_id,
                creator_id: r.creator_id,
                original_url: moekura_storage::Key::parse(&r.old_storage_key)
                    .map(|k| state.file_url(&k))
                    .unwrap_or_default(),
                replacement_url: r.source,
                md5_was: hex::encode(&r.old_md5),
                md5: hex::encode(&r.new_md5),
                file_ext_was: ext(&r.old_media_type),
                file_ext: ext(&r.new_media_type),
                image_width_was: r.old_width,
                image_height_was: r.old_height,
                image_width: r.new_width,
                image_height: r.new_height,
                file_size_was: r.old_file_size,
                file_size: r.new_file_size,
                updated_at: at.clone(),
                created_at: at,
            }
        })
        .collect();
    json(list, &params.list.only)
}
