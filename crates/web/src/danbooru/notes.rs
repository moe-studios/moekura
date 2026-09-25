//! Notes in Danbooru's shapes: `/notes.json`, `/notes/{id}.json` and
//! `/note_versions.json`. Read-only: notes are changed on the site or
//! through Moekura's API.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::notes::{self, Note, Version};
use serde::{Deserialize, Serialize};

use super::{ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::posts::visibility;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/notes", get(list))
        .route("/notes/{id}", get(show))
        .route("/note_versions", get(versions))
}

#[derive(Debug, Serialize)]
struct DanbooruNote {
    id: i64,
    post_id: i64,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    body: String,
    is_active: bool,
    version: i32,
    created_at: String,
    updated_at: String,
}

impl From<Note> for DanbooruNote {
    fn from(n: Note) -> Self {
        Self {
            id: n.id,
            post_id: n.post_id,
            x: n.x,
            y: n.y,
            width: n.width,
            height: n.height,
            body: n.body,
            is_active: n.is_active,
            version: n.version,
            created_at: timestamp(n.created_at),
            updated_at: timestamp(n.updated_at),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct NoteParams {
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(rename = "search[is_active]", default)]
    is_active: String,
    #[serde(flatten)]
    list: ListParams,
}

/// Ids from a list separated by spaces or commas.
fn ids(value: &str) -> Vec<i64> {
    value
        .split([' ', ','])
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

/// The notes of the posts asked for (clients always ask by post), on
/// posts the requester can see.
async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<NoteParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let asked = ids(&params.post_id);
    let visible: Vec<i64> = moekura_db::posts::by_ids(db, &asked)
        .await?
        .into_iter()
        .filter(|p| visibility(&current).allows(p))
        .map(|p| p.id)
        .collect();
    let limit = params.list.limit(1000) as usize;
    let mut found = if params.is_active.trim() == "false" {
        let mut all = Vec::new();
        for id in &visible {
            all.extend(
                notes::for_post(db, *id, true)
                    .await?
                    .into_iter()
                    .filter(|n| !n.is_active),
            );
        }
        all
    } else {
        notes::for_posts(db, &visible).await?
    };
    found.truncate(limit);
    json(
        found
            .into_iter()
            .map(DanbooruNote::from)
            .collect::<Vec<_>>(),
        &params.list.only,
    )
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(list): Query<ListParams>,
) -> Result<Response, AppError> {
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let (note, _, _) = crate::notes::visible_note(&state, &current, id).await?;
    json(DanbooruNote::from(note), &list.only)
}

#[derive(Debug, Serialize)]
struct DanbooruNoteVersion {
    /// Versions have no ids of their own here: the note's.
    id: i64,
    note_id: i64,
    post_id: i64,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    body: String,
    is_active: bool,
    version: i32,
    created_at: String,
    updated_at: String,
}

impl From<Version> for DanbooruNoteVersion {
    fn from(v: Version) -> Self {
        let at = timestamp(v.created_at);
        Self {
            id: v.note_id,
            note_id: v.note_id,
            post_id: v.post_id,
            x: v.x,
            y: v.y,
            width: v.width,
            height: v.height,
            body: v.body,
            is_active: v.is_active,
            version: v.version,
            updated_at: at.clone(),
            created_at: at,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct VersionParams {
    #[serde(rename = "search[post_id]", default)]
    post_id: String,
    #[serde(rename = "search[note_id]", default)]
    note_id: String,
    #[serde(flatten)]
    list: ListParams,
}

/// Note versions of one post or note, newest first.
async fn versions(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<VersionParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let note_id = params.note_id.trim().parse::<i64>().ok();
    let post_id = match (params.post_id.trim().parse::<i64>().ok(), note_id) {
        (Some(post), _) => post,
        (None, Some(note)) => {
            crate::notes::visible_note(&state, &current, note)
                .await?
                .0
                .post_id
        }
        // Only for one post or note: the history of hidden posts stays
        // hidden.
        (None, None) => return json(Vec::<DanbooruNoteVersion>::new(), ""),
    };
    crate::notes::visible_post(&state, &current, post_id).await?;
    let limit = i64::from(params.list.limit(1000));
    let found =
        notes::versions(state.reader(&current), note_id, Some(post_id), None, limit).await?;
    json(
        found
            .into_iter()
            .map(DanbooruNoteVersion::from)
            .collect::<Vec<_>>(),
        &params.list.only,
    )
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::notes::NoteBox;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn notes(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let post = upload(&app, &alice, 20, "cat").await;
        let note_box = NoteBox {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        let id = moekura_db::notes::create(&pool, post, note_box, "Hello", None)
            .await
            .unwrap();

        let listed: Value = serde_json::from_str(
            &app.get(&format!("/notes.json?search[post_id]={post}"), None)
                .await
                .body,
        )
        .unwrap();
        assert_eq!(
            (&listed[0]["id"], &listed[0]["body"], &listed[0]["width"]),
            (&json!(id), &json!("Hello"), &json!(3))
        );
        assert_eq!(app.get("/notes.json", None).await.body, "[]");
        let one = app.get(&format!("/notes/{id}.json?only=x,y"), None).await;
        assert_eq!(one.body, r#"{"x":1,"y":2}"#);
        let versions: Value = serde_json::from_str(
            &app.get(&format!("/note_versions.json?search[note_id]={id}"), None)
                .await
                .body,
        )
        .unwrap();
        assert_eq!(versions[0]["version"], json!(1));
        assert_eq!(
            app.get("/notes/999.json", None).await.status,
            StatusCode::NOT_FOUND
        );
        let post_json: Value =
            serde_json::from_str(&app.get(&format!("/posts/{post}.json"), None).await.body)
                .unwrap();
        assert!(post_json["last_noted_at"].is_string());
    }
}
