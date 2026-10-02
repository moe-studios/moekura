//! Danbooru's two-step uploads: `POST /uploads.json` stores a file (or
//! fetches `upload[source]`) as an upload of one staged file, and
//! `POST /posts.json` with `upload_media_asset_id` makes it a post. A
//! staged file is both the upload media asset and its media asset, so
//! those share its id; the upload has its own.

use axum::Router;
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_core::posts::SOURCE_MAX_LEN;
use moekura_db::staged_uploads::{self, Staged, Status, Upload};
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
        UploadError::Similar(found) => AppError::Similar {
            posts: found.posts,
            staged: found.staged,
        },
        UploadError::Limit(message) => AppError::Blocked(message),
        UploadError::Internal(detail) => AppError::Internal(detail),
    }
}

/// A staged file as Danbooru's upload media asset, with its media asset
/// once it's stored.
fn asset_json(staged: &Staged) -> Value {
    let created = timestamp(staged.created_at);
    let updated = timestamp(staged.updated_at);
    let media_asset = staged.status.eq(&Status::Ready).then(|| {
        let ext = match staged.media_type.as_deref().unwrap_or_default() {
            "jpeg" => "jpg",
            other => other,
        };
        json!({
            "id": staged.id,
            "created_at": created,
            "updated_at": updated,
            "md5": staged.md5.as_deref().map(hex::encode),
            "file_ext": ext,
            "file_size": staged.file_size,
            "image_width": staged.width,
            "image_height": staged.height,
            "duration": staged.duration_ms.map(|ms| f64::from(ms) / 1000.0),
            "status": "active",
            "is_public": true,
            "variants": [],
        })
    });
    let status = match staged.status {
        Status::Pending => "processing",
        Status::Ready => "active",
        Status::Failed => "failed",
    };
    json!({
        "id": staged.id,
        "created_at": created,
        "updated_at": updated,
        "upload_id": staged.upload_id,
        "media_asset_id": media_asset.as_ref().map(|_| staged.id),
        "status": status,
        "source_url": staged.file_url.as_deref().unwrap_or(&staged.source),
        "page_url": (!staged.source.is_empty()).then_some(&staged.source),
        "error": staged.error,
        "post_id": staged.post_id.or(staged.duplicate_of),
        "media_asset": media_asset,
    })
}

/// An upload and its files, as Danbooru describes them.
fn upload_json(upload: &Upload, files: &[Staged]) -> Value {
    let created = timestamp(upload.created_at);
    let status = if files.iter().any(|f| f.status == Status::Pending) {
        "processing"
    } else if !files.is_empty() && files.iter().all(|f| f.status == Status::Failed) {
        "error"
    } else {
        "completed"
    };
    let error = (status == "error")
        .then(|| files.iter().find_map(|f| f.error.clone()))
        .flatten();
    json!({
        "id": upload.id,
        "source": upload.source,
        "uploader_id": upload.uploader_id,
        "status": status,
        "media_asset_count": files.iter().filter(|f| f.status == Status::Ready).count(),
        "created_at": created,
        "updated_at": created,
        "referer_url": (!upload.referer_url.is_empty()).then_some(&upload.referer_url),
        "error": error,
        "upload_media_assets": files.iter().map(asset_json).collect::<Vec<_>>(),
    })
}

/// What `POST /uploads.json` sends.
#[derive(Default)]
struct Sent {
    file: Option<TempUpload>,
    source: String,
    /// The page the source was found on.
    referer: String,
}

/// A file from `upload[files][0]` (or any `upload[files]…` part), or the
/// source to fetch, and the page it's from.
async fn receive(state: &AppState, request: Request) -> Result<Sent, AppError> {
    let multipart = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("multipart/form-data"));
    let clean = |text: &str| -> String { text.trim().chars().take(SOURCE_MAX_LEN).collect() };
    if !multipart {
        let fields = Fields::from_request(request, state).await?;
        let source = fields
            .get("upload[source]")
            .or_else(|| fields.get("upload[source_url]"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let referer = clean(fields.get("upload[referer_url]").unwrap_or_default());
        return Ok(Sent {
            file: None,
            source,
            referer,
        });
    }
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let mut sent = Sent::default();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        if name.starts_with("upload[files]") || name == "upload[file]" {
            if sent.file.is_none() && field.file_name().is_some_and(|f| !f.is_empty()) {
                sent.file = Some(save_to_temp(state, field).await.map_err(upload_error)?);
            }
        } else if matches!(
            name.as_str(),
            "upload[source]" | "upload[source_url]" | "upload[referer_url]"
        ) {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::BadRequest(e.to_string()))?;
            if name == "upload[referer_url]" {
                sent.referer = clean(&text);
            } else {
                sent.source = text.trim().to_owned();
            }
        }
    }
    Ok(sent)
}

async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    request: Request,
) -> Result<Response, AppError> {
    current.require(Permission::Upload)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    check_limits(&state, &current).await.map_err(upload_error)?;
    let Sent {
        file,
        source: link,
        referer,
    } = receive(&state, request).await?;
    let mut source = link.clone();
    let file = match file {
        Some(file) => file,
        None if !link.is_empty() => {
            let mut fields = UploadFields {
                url: link.clone(),
                referer: referer.clone(),
                ..UploadFields::default()
            };
            let file = fetch_url(&state, &mut fields).await.map_err(upload_error)?;
            source = fields.source;
            file
        }
        None => {
            return Err(AppError::Unprocessable(
                "Send a file as upload[files][0], or a link as upload[source]".into(),
            ));
        }
    };
    let prepared = prepare(&state, &file).await.map_err(upload_error)?;
    let hash = crate::upload::phash(&state, &file).await;
    let origin = crate::upload::Origin {
        link: &link,
        source: &source,
        referer: &referer,
    };
    let (id, _) = crate::upload::stage(&state, user.id, &file, &prepared, hash, origin)
        .await
        .map_err(upload_error)?;
    let (upload, files) = own_upload(&state, &current, id).await?;
    tracing::info!(id, user = user.name, "upload staged");
    Ok((
        StatusCode::CREATED,
        axum::Json(upload_json(&upload, &files)),
    )
        .into_response())
}

/// Upload `id` and its files, if it's `current`'s.
async fn own_upload(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<(Upload, Vec<Staged>), AppError> {
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = state.db.primary();
    let upload = staged_uploads::upload_by_id(db, id)
        .await?
        .filter(|u| u.uploader_id == user.id)
        .ok_or(AppError::NotFound)?;
    let files = staged_uploads::of_upload(db, id).await?;
    Ok((upload, files))
}

/// Staged file `id`, if it's `current`'s.
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
    let (upload, files) = own_upload(&state, &current, id).await?;
    respond(upload_json(&upload, &files), &list.only)
}

/// The user's uploads, newest first. Files not posted are only kept for
/// a day.
async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(list): Query<ListParams>,
) -> Result<Response, AppError> {
    let Some(user) = &current.user else {
        return respond(Vec::<Value>::new(), &list.only);
    };
    let db = state.db.primary();
    let limit = i64::from(list.limit(100));
    let page = list.page.trim().parse::<i64>().unwrap_or(1).clamp(1, 1000);
    let uploads =
        staged_uploads::uploads_by_uploader(db, user.id, (page - 1) * limit, limit).await?;
    let mut found = Vec::with_capacity(uploads.len());
    for upload in &uploads {
        let files = staged_uploads::of_upload(db, upload.id).await?;
        found.push(upload_json(upload, &files));
    }
    respond(found, &list.only)
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
    let id = |name: &str| fields.get(name).and_then(|v| v.trim().parse::<i64>().ok());
    let staged = match (
        id("upload_media_asset_id").or_else(|| id("media_asset_id")),
        id("upload_id"),
    ) {
        (Some(file), _) => own(&state, &current, file).await?,
        // An upload's first file not posted yet.
        (None, Some(upload)) => own_upload(&state, &current, upload)
            .await?
            .1
            .into_iter()
            .find(|f| f.status == Status::Ready && f.post_id.is_none())
            .ok_or(AppError::NotFound)?,
        (None, None) => {
            return Err(AppError::Unprocessable(
                "`upload_media_asset_id` is required".into(),
            ));
        }
    };
    let id = staged.id;
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
    let prepared = Prepared::from_staged(&staged).ok_or(AppError::NotFound)?;
    crate::upload::check_new_uploader(&state, &current, id)
        .await
        .map_err(upload_error)?;
    let post_id = make_post(&state, &current, &prepared, &upload)
        .await
        .map_err(upload_error)?;
    staged_uploads::used(state.db.primary(), id, post_id).await?;
    let parent = field("parent_id");
    if !parent.trim().is_empty() {
        let set = match parent.trim().parse::<i64>() {
            Ok(parent) => crate::edit::set_parent(&state, &current, post_id, Some(parent)).await,
            Err(_) => Err(crate::edit::Refused::Invalid(
                "The parent must be a post number.".into(),
            )),
        };
        match set {
            Ok(()) => {}
            Err(crate::edit::Refused::Invalid(message)) => {
                tracing::warn!(post_id, message, "parent from a Danbooru upload not set");
            }
            Err(crate::edit::Refused::Error(error)) => return Err(error),
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
                &[(
                    "upload[referer_url]",
                    "https://example.com/gallery".to_owned(),
                )],
                "upload[files][0]",
                Some(("a.png", &png)),
            )
            .await;
        assert_eq!(staged.status, StatusCode::CREATED, "{}", staged.body);
        let upload = parse(&staged.body);
        assert_eq!(upload["referer_url"], json!("https://example.com/gallery"));
        let asset = &upload["upload_media_assets"][0];
        let id = asset["id"].as_i64().unwrap();
        let upload_id = upload["id"].as_i64().unwrap();
        assert_eq!(upload["status"], json!("completed"));
        assert_eq!(asset["upload_id"], json!(upload_id));
        assert_eq!(
            (
                &asset["media_asset"]["file_ext"],
                &asset["media_asset"]["image_width"]
            ),
            (&json!("png"), &json!(40))
        );
        assert_eq!(
            parse(
                &app.get(&format!("/uploads/{upload_id}.json"), Some(&alice))
                    .await
                    .body
            )["upload_media_assets"][0]["id"],
            json!(id)
        );
        assert_eq!(
            parse(&app.get("/uploads.json", Some(&alice)).await.body)[0]["id"],
            json!(upload_id)
        );
        // Only the uploader sees or posts it.
        assert_eq!(
            app.get(&format!("/uploads/{upload_id}.json"), Some(&bob))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            parse(&app.get("/uploads.json", Some(&bob)).await.body),
            json!([])
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
        let posted_asset = &parse(
            &app.get(&format!("/uploads/{upload_id}.json"), Some(&alice))
                .await
                .body,
        )["upload_media_assets"][0];
        assert_eq!(posted_asset["post_id"], post["id"]);

        // An upload's number posts its first file not yet posted.
        let other = app
            .post_multipart_as(
                "/uploads.json",
                Some(&alice),
                &[],
                "upload[files][0]",
                Some(("b.png", &fixture::png(44, 30))),
            )
            .await;
        let other = parse(&other.body)["id"].as_i64().unwrap();
        let by_upload = app
            .post_form(
                "/posts.json",
                Some(&alice),
                &[],
                &format!("upload_id={other}&post[rating]=g"),
            )
            .await;
        assert_eq!(by_upload.status, StatusCode::CREATED, "{}", by_upload.body);
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
