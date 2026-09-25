//! Notes on posts.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use moekura_core::markup;
use moekura_core::notes::NoteBox;
use moekura_db::notes::{self, Changes, Note};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::{AppError, ErrorBody};
use crate::notes::{create as create_note, update as update_note, visible_note};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiNote {
    pub id: i64,
    pub post_id: i64,
    /// The box, in the original image's pixels.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// In the site's wiki markup.
    pub body: String,
    /// `body` as HTML.
    pub html: String,
    /// False once deleted.
    pub is_active: bool,
    /// Counts up with every change; send it back when changing the note.
    pub version: i32,
    pub updater: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<Note> for ApiNote {
    fn from(n: Note) -> Self {
        Self {
            html: markup::render(&n.body),
            id: n.id,
            post_id: n.post_id,
            x: n.x,
            y: n.y,
            width: n.width,
            height: n.height,
            body: n.body,
            is_active: n.is_active,
            version: n.version,
            updater: n.updater_name,
            created_at: n.created_at,
            updated_at: n.updated_at,
        }
    }
}

async fn one(state: &AppState, id: i64) -> Result<ApiNote, AppError> {
    Ok(notes::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?
        .into())
}

/// Get a note.
///
/// Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/notes/{id}",
    operation_id = "get_note",
    tag = "notes",
    params(("id" = i64, Path, description = "Note number")),
    responses((status = 200, body = ApiNote), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<ApiNote>, AppError> {
    let (note, _, _) = visible_note(&state, &current, id).await?;
    Ok(Json(note.into()))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewNote {
    /// The box, in the original image's pixels; trimmed to the image.
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    /// In the site's wiki markup; at most 10,000 characters.
    body: String,
}

/// Add a note to a post.
///
/// Needs `edit_notes`. Not on deleted posts; at most 500 per post.
#[utoipa::path(
    post,
    path = "/posts/{id}/notes",
    operation_id = "create_note",
    tag = "notes",
    params(("id" = i64, Path, description = "Post number")),
    request_body = NewNote,
    responses(
        (status = 201, body = ApiNote),
        (status = 404, body = ErrorBody),
        (status = 422, body = ErrorBody, description = "Empty text, or a box outside the image"),
    ),
)]
pub(crate) async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(new): Json<NewNote>,
) -> Result<(StatusCode, Json<ApiNote>), AppError> {
    let note_box = NoteBox {
        x: new.x,
        y: new.y,
        width: new.width,
        height: new.height,
    };
    let note_id = create_note(&state, &current, id, note_box, &new.body).await?;
    Ok((StatusCode::CREATED, Json(one(&state, note_id).await?)))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteChanges {
    /// A new box: all four or none.
    x: Option<i32>,
    y: Option<i32>,
    width: Option<i32>,
    height: Option<i32>,
    body: Option<String>,
    /// `false` deletes the note, `true` restores it.
    is_active: Option<bool>,
    /// The version the changes were based on. If someone has changed the
    /// note since, the change is refused with 409. Leave it out to change
    /// whatever is there.
    base_version: Option<i32>,
}

/// Change a note.
///
/// Needs `edit_notes`. Fields left out stay as they are.
#[utoipa::path(
    put,
    path = "/notes/{id}",
    operation_id = "update_note",
    tag = "notes",
    params(("id" = i64, Path, description = "Note number")),
    request_body = NoteChanges,
    responses(
        (status = 200, body = ApiNote),
        (status = 409, body = ErrorBody, description = "The note changed since `base_version`"),
        (status = 422, body = ErrorBody),
    ),
)]
pub(crate) async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(changes): Json<NoteChanges>,
) -> Result<Json<ApiNote>, AppError> {
    let note_box = match (changes.x, changes.y, changes.width, changes.height) {
        (Some(x), Some(y), Some(width), Some(height)) => Some(NoteBox {
            x,
            y,
            width,
            height,
        }),
        (None, None, None, None) => None,
        _ => {
            return Err(AppError::Unprocessable(
                "Send all of x, y, width and height, or none.".into(),
            ));
        }
    };
    let changes_to_make = Changes {
        note_box,
        body: changes.body,
        is_active: changes.is_active,
    };
    update_note(&state, &current, id, changes_to_make, changes.base_version).await?;
    Ok(Json(one(&state, id).await?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteParams {
    /// Refuse with 409 if the note changed since this version.
    base_version: Option<i32>,
}

/// Delete a note.
///
/// Needs `edit_notes`. It stays in the history and can be restored.
#[utoipa::path(
    delete,
    path = "/notes/{id}",
    operation_id = "delete_note",
    tag = "notes",
    params(("id" = i64, Path, description = "Note number"), DeleteParams),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
)]
pub(crate) async fn delete(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Query(params): Query<DeleteParams>,
) -> Result<StatusCode, AppError> {
    let changes = Changes {
        is_active: Some(false),
        ..Changes::default()
    };
    update_note(&state, &current, id, changes, params.base_version).await?;
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
    async fn notes(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let post = upload(&app, &alice, &fixture::png(40, 20), "cat").await;
        let create = format!("/api/v1/posts/{post}/notes");
        let body =
            json!({ "x": 30, "y": -5, "width": 20, "height": 10, "body": "Hi [b]there[/b]" });

        assert_eq!(
            app.json("POST", &create, None, Some(body.clone()))
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
        let created = app.json("POST", &create, Some(&alice), Some(body)).await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let note = json(&created.body);
        // Trimmed to the 40×20 image.
        assert_eq!(
            (&note["x"], &note["y"], &note["width"], &note["height"]),
            (&json!(30), &json!(0), &json!(10), &json!(5))
        );
        assert_eq!(note["html"], json!("<p>Hi <strong>there</strong></p>"));
        let id = note["id"].as_i64().unwrap();
        let outside = app
            .json(
                "POST",
                &create,
                Some(&alice),
                Some(json!({ "x": 50, "y": 0, "width": 5, "height": 5, "body": "x" })),
            )
            .await;
        assert_eq!(outside.status, StatusCode::UNPROCESSABLE_ENTITY);

        let note_url = format!("/api/v1/notes/{id}");
        let moved = app
            .json(
                "PUT",
                &note_url,
                Some(&alice),
                Some(json!({ "x": 0, "y": 0, "width": 4, "height": 4, "base_version": 1 })),
            )
            .await;
        assert_eq!(json(&moved.body)["version"], json!(2), "{}", moved.body);
        let stale = app
            .json(
                "PUT",
                &note_url,
                Some(&alice),
                Some(json!({ "body": "Old", "base_version": 1 })),
            )
            .await;
        assert_eq!(stale.status, StatusCode::CONFLICT);
        let half = app
            .json("PUT", &note_url, Some(&alice), Some(json!({ "x": 1 })))
            .await;
        assert_eq!(half.status, StatusCode::UNPROCESSABLE_ENTITY);

        assert_eq!(
            app.json(
                "DELETE",
                &format!("{note_url}?base_version=2"),
                Some(&alice),
                None
            )
            .await
            .status,
            StatusCode::NO_CONTENT
        );
        let shown = json(&app.get(&note_url, None).await.body);
        assert_eq!(
            (&shown["is_active"], &shown["version"]),
            (&json!(false), &json!(3))
        );
    }
}
