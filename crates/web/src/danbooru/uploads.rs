//! Danbooru's two-step uploads: `POST /uploads.json` stores a file (or
//! fetches `upload[source]`) as a staged upload, and `POST /posts.json`
//! with `upload_media_asset_id` makes it a post. Staged uploads, their
//! upload media assets and media assets share one id here.

use axum::Router;
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::staged_uploads::{self, NewStaged, Staged};
use serde_json::{Value, json};

use super::{Fields, ListParams, json as respond, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::upload::{
    Prepared, TempUpload, UploadError, UploadFields, check_limits, create_post as make_post,
    fetch_url, prepare, save_to_temp,
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/uploads", get(list).post(create))
        .route("/uploads/{id}", get(show))
}

fn upload_error(error: UploadError) -> AppError {
    match error {
        UploadError::Invalid(message) => AppError::Unprocessable(message),
        UploadError::Duplicate(id) => AppError::Duplicate(id),
        UploadError::Limit(message) => AppError::Blocked(message),
        UploadError::Internal(detail) => AppError::Internal(detail),
    }
}

/// The upload, with its one upload media asset and media asset, as
/// Danbooru describes them.
fn upload_json(staged: &Staged) -> Value {
    let created = timestamp(staged.created_at);
    let ext = if staged.media_type == "jpeg" {
        "jpg"
    } else {
        &staged.media_type
    };
    let media_asset = json!({
        "id": staged.id,
        "created_at": created,
        "updated_at": created,
        "md5": hex::encode(&staged.md5),
        "file_ext": ext,
        "file_size": staged.file_size,
        "image_width": staged.width,
        "image_height": staged.height,
        "duration": staged.duration_ms.map(|ms| f64::from(ms) / 1000.0),
        "status": "active",
        "is_public": true,
        "variants": [],
    });
    json!({
        "id": staged.id,
        "source": staged.source,
        "uploader_id": staged.uploader_id,
        "status": "completed",
        "media_asset_count": 1,
        "created_at": created,
        "updated_at": created,
        "referer_url": null,
        "error": null,
        "upload_media_assets": [{
            "id": staged.id,
            "created_at": created,
            "updated_at": created,
            "upload_id": staged.id,
            "media_asset_id": staged.id,
            "status": "active",
            "source_url": staged.source,
            "page_url": null,
            "error": null,
            "post_id": staged.post_id,
            "media_asset": media_asset,
        }],
    })
}

/// A file from `upload[files][0]` (or any `upload[files]…` part), or the
/// source to fetch.
async fn receive(
    state: &AppState,
    request: Request,
) -> Result<(Option<TempUpload>, String), AppError> {
    let multipart = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("multipart/form-data"));
    if !multipart {
        let fields = Fields::from_request(request, state).await?;
        let source = fields
            .get("upload[source]")
            .or_else(|| fields.get("upload[source_url]"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        return Ok((None, source));
    }
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let (mut file, mut source) = (None, String::new());
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        if name.starts_with("upload[files]") || name == "upload[file]" {
            if file.is_none() && field.file_name().is_some_and(|f| !f.is_empty()) {
                file = Some(save_to_temp(state, field).await.map_err(upload_error)?);
            }
        } else if name == "upload[source]" || name == "upload[source_url]" {
            source = field
                .text()
                .await
                .map_err(|e| AppError::BadRequest(e.to_string()))?
                .trim()
                .to_owned();
        }
    }
    Ok((file, source))
}

async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    request: Request,
) -> Result<Response, AppError> {
    current.require(Permission::Upload)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    check_limits(&state, &current).await.map_err(upload_error)?;
    let (file, source) = receive(&state, request).await?;
    let file = match file {
        Some(file) => file,
        None if !source.is_empty() => {
            let mut fields = UploadFields {
                url: source.clone(),
                ..UploadFields::default()
            };
            fetch_url(&state, &mut fields).await.map_err(upload_error)?
        }
        None => {
            return Err(AppError::Unprocessable(
                "Send a file as upload[files][0], or a link as upload[source]".into(),
            ));
        }
    };
    let prepared = prepare(&state, &file).await.map_err(upload_error)?;
    let id = staged_uploads::create(
        state.db.primary(),
        NewStaged {
            uploader_id: user.id,
            source: &source,
            sha256: &prepared.sha256,
            md5: &prepared.md5,
            media_type: &prepared.media_type,
            width: prepared.width,
            height: prepared.height,
            duration_ms: prepared.duration_ms,
            frames: prepared.frames,
            has_audio: prepared.has_audio,
            file_size: prepared.file_size,
            storage_key: &prepared.storage_key,
        },
    )
    .await?;
    let staged = staged_uploads::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    tracing::info!(id, user = user.name, "upload staged");
    Ok((StatusCode::CREATED, axum::Json(upload_json(&staged))).into_response())
}

/// Staged upload `id`, if it's `current`'s.
async fn own(state: &AppState, current: &CurrentUser, id: i64) -> Result<Staged, AppError> {
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    staged_uploads::by_id(state.db.primary(), id)
        .await?
        .filter(|s| s.uploader_id == user.id)
        .ok_or(AppError::NotFound)
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(list): Query<ListParams>,
) -> Result<Response, AppError> {
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let staged = own(&state, &current, id).await?;
    respond(upload_json(&staged), &list.only)
}

/// Uploads are only kept until they're posted, so there's no list.
async fn list() -> Result<Response, AppError> {
    respond(Vec::<Value>::new(), "")
}

/// `POST /posts.json`: makes the post of a staged upload
/// (`upload_media_asset_id`, or `media_asset_id` or `upload_id`) with
/// `post[tag_string]`, `post[rating]`, `post[source]`, and optionally
/// `post[parent_id]` and `post[description]`.
pub(super) async fn create_post(
    State(state): State<AppState>,
    current: CurrentUser,
    fields: Fields,
) -> Result<Response, AppError> {
    current.require(Permission::Upload)?;
    let id: i64 = ["upload_media_asset_id", "media_asset_id", "upload_id"]
        .iter()
        .find_map(|name| fields.get(name).and_then(|v| v.trim().parse().ok()))
        .ok_or_else(|| AppError::Unprocessable("`upload_media_asset_id` is required".into()))?;
    let staged = own(&state, &current, id).await?;
    if let Some(post) = staged.post_id {
        return Err(AppError::Duplicate(post));
    }
    check_limits(&state, &current).await.map_err(upload_error)?;
    let field = |name: &str| {
        fields
            .get(&format!("post[{name}]"))
            .unwrap_or_default()
            .to_owned()
    };
    let source = match field("source") {
        source if source.is_empty() => staged.source.clone(),
        source => source,
    };
    let upload = UploadFields {
        rating: field("rating").parse().ok(),
        tags: field("tag_string"),
        source,
        description: field("description"),
        ..UploadFields::default()
    };
    let prepared = Prepared {
        sha256: staged
            .sha256
            .as_slice()
            .try_into()
            .map_err(|_| AppError::NotFound)?,
        md5: staged
            .md5
            .as_slice()
            .try_into()
            .map_err(|_| AppError::NotFound)?,
        media_type: staged.media_type.clone(),
        width: staged.width,
        height: staged.height,
        duration_ms: staged.duration_ms,
        frames: staged.frames,
        has_audio: staged.has_audio,
        file_size: staged.file_size,
        storage_key: staged.storage_key.clone(),
    };
    let post_id = make_post(&state, &current, &prepared, &upload)
        .await
        .map_err(upload_error)?;
    staged_uploads::used(state.db.primary(), id, post_id).await?;
    let parent = field("parent_id");
    if !parent.trim().is_empty() {
        let form = crate::edit::EditForm {
            tags: upload.tags.clone(),
            old_tags: String::new(),
            rating: upload
                .rating
                .map(|r| r.code().to_owned())
                .unwrap_or_default(),
            source: upload.source.clone(),
            description: upload.description.clone(),
            parent,
            ..Default::default()
        };
        if let Err(crate::edit::Refused::Invalid(message)) =
            crate::edit::apply(&state, &current, post_id, &form).await
        {
            tracing::warn!(post_id, message, "parent from a Danbooru upload not set");
        }
    }
    let db = state.db.primary();
    let post = moekura_db::posts::by_id(db, post_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let mut found = super::posts::danbooru_posts(&state, db, vec![post]).await?;
    let post = found.pop().ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, axum::Json(post)).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::app;
    use crate::test_support::{fixture, session_for};

    fn parse(body: &str) -> Value {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn two_step_uploads(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let png = fixture::png(40, 30);

        let staged = app
            .post_multipart_as(
                "/uploads.json",
                Some(&alice),
                &[],
                "upload[files][0]",
                Some(("a.png", &png)),
            )
            .await;
        assert_eq!(staged.status, StatusCode::CREATED, "{}", staged.body);
        let upload = parse(&staged.body);
        let asset = &upload["upload_media_assets"][0];
        let id = asset["id"].as_i64().unwrap();
        assert_eq!(upload["status"], json!("completed"));
        assert_eq!(
            (
                &asset["media_asset"]["file_ext"],
                &asset["media_asset"]["image_width"]
            ),
            (&json!("png"), &json!(40))
        );
        assert_eq!(
            parse(
                &app.get(&format!("/uploads/{id}.json"), Some(&alice))
                    .await
                    .body
            )["id"],
            json!(id)
        );
        // Only the uploader sees or posts it.
        assert_eq!(
            app.get(&format!("/uploads/{id}.json"), Some(&bob))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.post_form(
                "/posts.json",
                Some(&bob),
                &[],
                &format!("upload_media_asset_id={id}&post[rating]=g")
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );
        let no_rating = app
            .post_form(
                "/posts.json",
                Some(&alice),
                &[],
                &format!("upload_media_asset_id={id}"),
            )
            .await;
        assert_eq!(no_rating.status, StatusCode::UNPROCESSABLE_ENTITY);

        let posted = app
            .post_form(
                "/posts.json",
                Some(&alice),
                &[],
                &format!("upload_media_asset_id={id}&post[tag_string]=cat+solo&post[rating]=s&post[source]=https%3A%2F%2Fexample.com%2F1"),
            )
            .await;
        assert_eq!(posted.status, StatusCode::CREATED, "{}", posted.body);
        let post = parse(&posted.body);
        assert_eq!(
            (&post["tag_string"], &post["rating"], &post["source"]),
            (
                &json!("cat solo"),
                &json!("s"),
                &json!("https://example.com/1")
            )
        );
        // Once only; the same file again is a duplicate.
        let again = app
            .post_form(
                "/posts.json",
                Some(&alice),
                &[],
                &format!("upload_media_asset_id={id}&post[rating]=g"),
            )
            .await;
        assert_eq!(again.status, StatusCode::CONFLICT);
        let duplicate = app
            .post_multipart_as(
                "/uploads.json",
                Some(&alice),
                &[],
                "upload[files][0]",
                Some(("a.png", &png)),
            )
            .await;
        assert_eq!(duplicate.status, StatusCode::CONFLICT);
        assert_eq!(
            app.post_form("/uploads.json", Some(&alice), &[], "")
                .await
                .status,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
