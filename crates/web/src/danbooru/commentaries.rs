//! `/artist_commentaries.json` (and one post's, `/posts/{id}/
//! artist_commentary.json`), `/artist_commentary_versions.json`, and
//! `create_or_update`. A commentary's `id` is its post's.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::{get, put};
use moekura_core::permissions::Permission;
use moekura_db::artist_commentaries::{self, Commentary, Texts, VersionFilter};
use serde::{Deserialize, Serialize};

use super::tags::window;
use super::{Fields, ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::posts::visibility;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/artist_commentaries", get(index))
        .route(
            "/artist_commentaries/create_or_update",
            put(create_or_update).post(create_or_update),
        )
        .route("/artist_commentaries/{id}", get(show))
        .route("/posts/{id}/artist_commentary", get(show))
        .route("/artist_commentary_versions", get(versions))
}

#[derive(Debug, Serialize)]
struct DanbooruCommentary {
    id: i64,
    post_id: i64,
    original_title: String,
    original_description: String,
    translated_title: String,
    translated_description: String,
    created_at: String,
    updated_at: String,
}

impl From<Commentary> for DanbooruCommentary {
    fn from(c: Commentary) -> Self {
        Self {
            id: c.post_id,
            post_id: c.post_id,
            original_title: c.texts.original_title,
            original_description: c.texts.original_description,
            translated_title: c.texts.translated_title,
            translated_description: c.texts.translated_description,
            created_at: timestamp(c.created_at),
            updated_at: timestamp(c.updated_at),
        }
    }
}

fn flag(value: &str) -> Option<bool> {
    match value.trim() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// Only those on posts `current` may see.
async fn visible(
    state: &AppState,
    current: &CurrentUser,
    found: Vec<Commentary>,
) -> Result<Vec<Commentary>, AppError> {
    let ids: Vec<i64> = found.iter().map(|c| c.post_id).collect();
    let posts = moekura_db::posts::by_ids(state.reader(current), &ids).await?;
    let visible = visibility(current);
    Ok(found
        .into_iter()
        .filter(|c| posts.iter().any(|p| p.id == c.post_id && visible.allows(p)))
        .collect())
}

#[derive(Debug, Default, Deserialize)]
struct IndexParams {
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(rename = "search[text_matches]", default)]
    text_matches: String,
    #[serde(rename = "search[original_present]", default)]
    original_present: String,
    #[serde(rename = "search[translated_present]", default)]
    translated_present: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<IndexParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let (offset, limit) = window(&params.list, 1000)?;
    let post_ids: Option<Vec<i64>> = match params.post_id.trim() {
        "" => None,
        list => Some(
            list.split(',')
                .filter_map(|i| i.trim().parse().ok())
                .collect(),
        ),
    };
    let text = params.text_matches.replace('*', " ");
    let filter = artist_commentaries::Filter {
        post_ids: post_ids.as_deref(),
        text: text.trim(),
        original_present: flag(&params.original_present),
        translated_present: flag(&params.translated_present),
    };
    let found = artist_commentaries::list(state.reader(&current), &filter, offset, limit).await?;
    let list: Vec<DanbooruCommentary> = visible(&state, &current, found)
        .await?
        .into_iter()
        .map(DanbooruCommentary::from)
        .collect();
    json(list, &params.list.only)
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let found = artist_commentaries::for_post(state.reader(&current), id)
        .await?
        .into_iter()
        .collect();
    let commentary = visible(&state, &current, found)
        .await?
        .pop()
        .ok_or(AppError::NotFound)?;
    json(DanbooruCommentary::from(commentary), "")
}

/// Sets a post's commentary: `artist_commentary[post_id]` and the texts
/// given; texts left out keep their value.
async fn create_or_update(
    State(state): State<AppState>,
    current: CurrentUser,
    fields: Fields,
) -> Result<Response, AppError> {
    let post_id: i64 = fields
        .get("artist_commentary[post_id]")
        .or_else(|| fields.get("post_id"))
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| {
            AppError::Unprocessable("`artist_commentary[post_id]` is required".into())
        })?;
    let db = state.db.primary();
    let before = artist_commentaries::for_post(db, post_id)
        .await?
        .map(|c| c.texts)
        .unwrap_or_default();
    let field = |name: &str, old: &str| {
        fields
            .get(&format!("artist_commentary[{name}]"))
            .map_or_else(|| old.to_owned(), str::to_owned)
    };
    let texts = Texts {
        original_title: field("original_title", &before.original_title),
        original_description: field("original_description", &before.original_description),
        translated_title: field("translated_title", &before.translated_title),
        translated_description: field("translated_description", &before.translated_description),
    };
    crate::commentary::save_commentary(&state, &current, post_id, texts, None).await?;
    let saved = artist_commentaries::for_post(db, post_id)
        .await?
        .ok_or(AppError::NotFound)?;
    json(DanbooruCommentary::from(saved), "")
}

#[derive(Debug, Serialize)]
struct DanbooruCommentaryVersion {
    id: i64,
    post_id: i64,
    updater_id: Option<i64>,
    original_title: String,
    original_description: String,
    translated_title: String,
    translated_description: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Default, Deserialize)]
struct VersionParams {
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(rename = "search[updater_id]", default)]
    updater_id: String,
    #[serde(rename = "search[updater_name]", default)]
    updater_name: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn versions(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<VersionParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let (offset, limit) = window(&params.list, 1000)?;
    let post_id = match params.post_id.trim() {
        "" => None,
        id => Some(id.parse().unwrap_or(-1)),
    };
    let updater_id = match (params.updater_id.trim(), params.updater_name.trim()) {
        ("", "") => None,
        ("", name) => Some(
            moekura_db::users::by_name(db, name)
                .await?
                .map_or(-1, |u| u.id),
        ),
        (id, _) => Some(id.parse().unwrap_or(-1)),
    };
    let filter = VersionFilter {
        post_id,
        updater_id,
        before: None,
        offset,
    };
    let found = artist_commentaries::versions(db, filter, limit).await?;
    let ids: Vec<i64> = found.iter().map(|v| v.post_id).collect();
    let posts = moekura_db::posts::by_ids(db, &ids).await?;
    let visible = visibility(&current);
    let list: Vec<DanbooruCommentaryVersion> = found
        .into_iter()
        .filter(|v| posts.iter().any(|p| p.id == v.post_id && visible.allows(p)))
        .map(|v| {
            let at = timestamp(v.created_at);
            DanbooruCommentaryVersion {
                id: v.id,
                post_id: v.post_id,
                updater_id: v.updater_id,
                original_title: v.texts.original_title,
                original_description: v.texts.original_description,
                translated_title: v.texts.translated_title,
                translated_description: v.texts.translated_description,
                updated_at: at.clone(),
                created_at: at,
            }
        })
        .collect();
    json(list, &params.list.only)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn commentaries(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let post = upload(&app, &alice, 20, "cat").await;
        let body = json!({ "artist_commentary": {
            "post_id": post, "original_title": "猫", "translated_title": "Cat"
        }});
        let saved = app
            .json(
                "PUT",
                "/artist_commentaries/create_or_update.json",
                Some(&alice),
                Some(body),
            )
            .await;
        assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
        let parse = |body: &str| serde_json::from_str::<Value>(body).unwrap();
        assert_eq!(parse(&saved.body)["translated_title"], "Cat");

        let listed = parse(
            &app.get(
                "/artist_commentaries.json?search[translated_present]=yes",
                None,
            )
            .await
            .body,
        );
        assert_eq!(listed[0]["post_id"].as_i64(), Some(post));
        let one = parse(
            &app.get(&format!("/posts/{post}/artist_commentary.json"), None)
                .await
                .body,
        );
        assert_eq!(one["original_title"], "猫");
        let versions = parse(
            &app.get(
                &format!("/artist_commentary_versions.json?search[post_id]={post}"),
                None,
            )
            .await
            .body,
        );
        assert_eq!(versions.as_array().unwrap().len(), 1);
        assert_eq!(
            app.get("/artist_commentaries/999999.json", None)
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
}
