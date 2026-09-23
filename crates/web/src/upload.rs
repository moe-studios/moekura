//! Uploading posts.
//!
//! The file is streamed to a temporary file while it is hashed and
//! measured, then [`ingest`] identifies and probes it, stores the original
//! under its content hash, and creates the post, its media record and the
//! processing job in one transaction.

use std::path::{Path, PathBuf};

use axum::Router;
use axum::extract::multipart::{Field, MultipartError};
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use md5::Md5;
use minijinja::context;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uwuu_core::jobs::ProcessMedia;
use uwuu_core::permissions::Permission;
use uwuu_core::posts::{DESCRIPTION_MAX_LEN, PostStatus, Rating, SOURCE_MAX_LEN};
use uwuu_db::media::{self, InsertAssetError, NewAsset};
use uwuu_db::posts::{self, NewPost};
use uwuu_media::MediaError;
use uwuu_storage::Key;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::pages::Page;

/// Room for the text fields and multipart framing on top of the file.
const FORM_OVERHEAD: usize = 64 * 1024;

pub fn routes(max_upload_bytes: u64) -> Router<AppState> {
    let limit = usize::try_from(max_upload_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(FORM_OVERHEAD);
    Router::new().route(
        "/upload",
        get(upload_form)
            .post(upload)
            .layer(DefaultBodyLimit::max(limit)),
    )
}

/// The text fields of an upload.
#[derive(Debug, Default, Clone)]
pub struct UploadFields {
    /// Download the file from here when no file was sent.
    pub url: String,
    pub rating: Option<Rating>,
    /// Whitespace-separated, as typed.
    pub tags: String,
    pub source: String,
    pub description: String,
}

/// An uploaded file on local disk, removed when dropped.
pub struct TempUpload {
    path: PathBuf,
    pub sha256: [u8; 32],
    pub md5: [u8; 16],
    pub size: u64,
}

impl TempUpload {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempUpload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    /// Something the uploader can fix; shown on the form.
    #[error("{0}")]
    Invalid(String),
    #[error("this file was already uploaded as post #{0}")]
    Duplicate(i64),
    #[error("{0}")]
    Internal(String),
}

impl From<crate::tags::TagFieldError> for UploadError {
    fn from(error: crate::tags::TagFieldError) -> Self {
        match error {
            crate::tags::TagFieldError::Invalid(message) => Self::Invalid(message),
            crate::tags::TagFieldError::Db(error) => error.into(),
        }
    }
}

impl From<sqlx::Error> for UploadError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(format!("database: {error}"))
    }
}

fn max_bytes(state: &AppState) -> u64 {
    state.media.config().max_upload_mb * 1024 * 1024
}

async fn upload_form(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::Upload)?;
    Ok(render_form(
        &page,
        &UploadFields::default(),
        None,
        StatusCode::OK,
    ))
}

fn render_form(
    page: &Page,
    fields: &UploadFields,
    error: Option<&UploadError>,
    status: StatusCode,
) -> Response {
    let ratings: Vec<_> = Rating::ALL
        .iter()
        .map(|r| context! { code => r.code(), label => r.label() })
        .collect();
    let (message, duplicate_of) = match error {
        Some(UploadError::Duplicate(id)) => (None, Some(*id)),
        Some(e) => (Some(e.to_string()), None),
        None => (None, None),
    };
    page.render_with_status(
        status,
        "upload.html",
        context! {
            ratings => ratings,
            form => context! {
                rating => fields.rating.map(Rating::code),
                url => fields.url,
                tags => fields.tags,
                source => fields.source,
                description => fields.description,
            },
            error => message,
            duplicate_of => duplicate_of,
            max_mb => page.state().media.config().max_upload_mb,
        },
    )
}

async fn upload(
    State(state): State<AppState>,
    page: Page,
    multipart: Multipart,
) -> Result<Response, AppError> {
    page.current.require(Permission::Upload)?;
    let (mut fields, file) = match receive(&state, multipart).await {
        (fields, Ok(file)) => (fields, file),
        (fields, Err(error)) => return Ok(failed(&page, &fields, error)),
    };
    let file = match file {
        Some(file) => file,
        None if !fields.url.is_empty() => match fetch_url(&state, &mut fields).await {
            Ok(file) => file,
            Err(error) => return Ok(failed(&page, &fields, error)),
        },
        None => {
            let error = UploadError::Invalid("Choose a file to upload, or paste a link.".into());
            return Ok(failed(&page, &fields, error));
        }
    };
    match ingest(&state, &page.current, &file, &fields).await {
        Ok(post_id) => Ok(Redirect::to(&format!("/posts/{post_id}")).into_response()),
        Err(error) => Ok(failed(&page, &fields, error)),
    }
}

/// Downloads `fields.url`, which also becomes the source if none was given.
async fn fetch_url(state: &AppState, fields: &mut UploadFields) -> Result<TempUpload, UploadError> {
    let url = url::Url::parse(&fields.url)
        .map_err(|_| UploadError::Invalid("That isn't a valid link.".into()))?;
    let writer = TempWriter::create(&state.work_dir).await?;
    let file = state.fetcher.fetch(&url, writer, max_bytes(state)).await?;
    if fields.source.is_empty() {
        fields.source = fields.url.clone();
    }
    Ok(file)
}

fn failed(page: &Page, fields: &UploadFields, error: UploadError) -> Response {
    if let UploadError::Internal(detail) = &error {
        tracing::error!(error = %detail, "upload failed");
        let error =
            UploadError::Internal("Something went wrong on our side. Please try again.".into());
        return render_form(
            page,
            fields,
            Some(&error),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    render_form(page, fields, Some(&error), StatusCode::UNPROCESSABLE_ENTITY)
}

/// Reads the form, streaming the file to disk. The fields read so far come
/// back even on error, so the form can be shown again filled in.
async fn receive(
    state: &AppState,
    mut multipart: Multipart,
) -> (UploadFields, Result<Option<TempUpload>, UploadError>) {
    let mut fields = UploadFields::default();
    let mut file = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => return (fields, Err(multipart_error(state, &error))),
        };
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "file" => {
                // Browsers send an empty part when no file was chosen.
                if field.file_name().is_none_or(str::is_empty) {
                    continue;
                }
                match save_to_temp(state, field).await {
                    Ok(saved) => file = Some(saved),
                    Err(error) => return (fields, Err(error)),
                }
            }
            "url" | "rating" | "tags" | "source" | "description" => {
                let text = match field.text().await {
                    Ok(text) => text,
                    Err(error) => return (fields, Err(multipart_error(state, &error))),
                };
                match name.as_str() {
                    "url" => fields.url = text.trim().to_owned(),
                    "rating" => fields.rating = text.parse().ok(),
                    "tags" => fields.tags = text,
                    "source" => fields.source = text.trim().to_owned(),
                    _ => fields.description = text.trim().to_owned(),
                }
            }
            _ => {}
        }
    }
    (fields, Ok(file))
}

fn multipart_error(state: &AppState, error: &MultipartError) -> UploadError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        too_large(state)
    } else {
        UploadError::Invalid(format!(
            "The upload was interrupted or malformed ({})",
            error.body_text()
        ))
    }
}

fn too_large(state: &AppState) -> UploadError {
    UploadError::Invalid(format!(
        "The file is larger than {} MB.",
        state.media.config().max_upload_mb
    ))
}

async fn save_to_temp(state: &AppState, mut field: Field<'_>) -> Result<TempUpload, UploadError> {
    let limit = max_bytes(state);
    let mut writer = TempWriter::create(&state.work_dir).await?;
    loop {
        let chunk = match field.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => return Err(multipart_error(state, &error)),
        };
        if writer.written() + chunk.len() as u64 > limit {
            return Err(too_large(state));
        }
        writer.write(&chunk).await?;
    }
    writer.finish().await
}

/// Writes an upload to a temporary file while hashing it. Dropping it
/// before [`TempWriter::finish`] removes the partial file.
pub struct TempWriter {
    upload: TempUpload,
    file: tokio::fs::File,
    sha256: Sha256,
    md5: Md5,
}

impl TempWriter {
    pub async fn create(dir: &Path) -> Result<Self, UploadError> {
        let name = hex::encode(uwuu_core::tokens::NewToken::generate().hash);
        let upload = TempUpload {
            path: dir.join(format!("upload-{}", &name[..24])),
            sha256: [0; 32],
            md5: [0; 16],
            size: 0,
        };
        let file = tokio::fs::File::create(&upload.path)
            .await
            .map_err(|e| UploadError::Internal(format!("creating temp file: {e}")))?;
        Ok(Self {
            upload,
            file,
            sha256: Sha256::new(),
            md5: Md5::new(),
        })
    }

    pub fn written(&self) -> u64 {
        self.upload.size
    }

    pub async fn write(&mut self, chunk: &[u8]) -> Result<(), UploadError> {
        self.sha256.update(chunk);
        self.md5.update(chunk);
        self.upload.size += chunk.len() as u64;
        self.file
            .write_all(chunk)
            .await
            .map_err(|e| UploadError::Internal(format!("writing temp file: {e}")))
    }

    pub async fn finish(mut self) -> Result<TempUpload, UploadError> {
        self.file
            .flush()
            .await
            .map_err(|e| UploadError::Internal(format!("writing temp file: {e}")))?;
        if self.upload.size == 0 {
            return Err(UploadError::Invalid("The file is empty.".into()));
        }
        self.upload.sha256 = self.sha256.finalize().into();
        self.upload.md5 = self.md5.finalize().into();
        Ok(self.upload)
    }
}

/// Turns a received file into a post. Returns the new post's id.
pub async fn ingest(
    state: &AppState,
    uploader: &CurrentUser,
    file: &TempUpload,
    fields: &UploadFields,
) -> Result<i64, UploadError> {
    let rating = fields
        .rating
        .ok_or_else(|| UploadError::Invalid("Choose a rating.".into()))?;
    if fields.source.chars().count() > SOURCE_MAX_LEN {
        return Err(UploadError::Invalid(format!(
            "The source may be at most {SOURCE_MAX_LEN} characters."
        )));
    }
    if fields.description.chars().count() > DESCRIPTION_MAX_LEN {
        return Err(UploadError::Invalid(format!(
            "The description may be at most {DESCRIPTION_MAX_LEN} characters."
        )));
    }

    let db = state.db.primary();
    let tags = crate::tags::parse_field(db, &fields.tags).await?;
    if let Some(existing) = media::post_with_sha256(db, &file.sha256).await? {
        return Err(UploadError::Duplicate(existing));
    }
    let media_type = state
        .media
        .identify(file.path())
        .await
        .map_err(media_error)?;
    let probe = state
        .media
        .probe(file.path(), media_type)
        .await
        .map_err(media_error)?;

    let hash = hex::encode(file.sha256);
    let key = Key::original(&hash, media_type.extension());
    // Content-addressed: if the bytes are already stored (e.g. an earlier
    // attempt failed after storing), there is nothing to do.
    let stored = state
        .storage
        .exists(&key)
        .await
        .map_err(|e| UploadError::Internal(e.to_string()))?;
    if !stored {
        state
            .storage
            .put_file(&key, file.path())
            .await
            .map_err(|e| UploadError::Internal(e.to_string()))?;
    }

    let site = state.site.get();
    let status =
        if site.settings.upload_approval && !uploader.can(Permission::UploadWithoutApproval) {
            PostStatus::Pending
        } else {
            PostStatus::Active
        };
    let as_i32 = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);

    let mut tx = db.begin().await?;
    let tag_ids: Vec<i32> = uwuu_db::tags::for_post(
        &mut tx,
        &tags.wanted(),
        uploader.can(Permission::ManageTags),
    )
    .await?
    .iter()
    .map(|t| t.id)
    .collect();
    let post_id = posts::insert(
        &mut *tx,
        NewPost {
            uploader_id: uploader.user.as_ref().map(|u| u.id),
            rating,
            status,
            source: &fields.source,
            description: &fields.description,
            tag_ids: &tag_ids,
        },
    )
    .await?;
    let asset = NewAsset {
        post_id,
        sha256: &file.sha256,
        md5: &file.md5,
        media_type: media_type.name(),
        width: as_i32(probe.width),
        height: as_i32(probe.height),
        duration_ms: probe.duration_ms.map(as_i32),
        frames: as_i32(probe.frames),
        has_audio: probe.has_audio,
        file_size: i64::try_from(file.size).unwrap_or(i64::MAX),
        storage_key: key.as_str(),
    };
    let asset_id = match media::insert(&mut *tx, asset).await {
        Ok(id) => id,
        // Someone uploaded the same file a moment ago.
        Err(InsertAssetError::Duplicate(_)) => {
            drop(tx);
            let existing = media::post_with_sha256(db, &file.sha256)
                .await?
                .unwrap_or_default();
            return Err(UploadError::Duplicate(existing));
        }
        Err(InsertAssetError::Db(error)) => return Err(error.into()),
    };
    uwuu_db::jobs::enqueue(&mut tx, &ProcessMedia { asset_id }).await?;
    tx.commit().await?;

    tracing::info!(post_id, %media_type, size = file.size, ?status, "post uploaded");
    Ok(post_id)
}

fn media_error(error: MediaError) -> UploadError {
    if error.is_internal() {
        UploadError::Internal(error.to_string())
    } else {
        let message = error.to_string();
        let mut chars = message.chars();
        let capitalised = chars
            .next()
            .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect());
        UploadError::Invalid(format!("{capitalised}."))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;
    use sqlx::PgPool;
    use uwuu_core::permissions::SystemRole;
    use uwuu_db::{jobs, settings};

    use axum::routing::get;

    use super::*;
    use crate::test_support::{TestApp, fixture, session_for, test_state};

    async fn app(pool: &PgPool) -> (TestApp, AppState) {
        let state = test_state(pool).await;
        let routes = routes(max_bytes(&state)).merge(crate::posts::routes());
        (TestApp::new(state.clone(), routes), state)
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn uploads_from_a_link_and_uses_it_as_source(pool: PgPool) {
        // A local server stands in for the web; the fetcher that allows
        // private addresses is only ever built by tests.
        let png = fixture::png(24, 24);
        let served = png.clone();
        let origin = Router::new().route("/art.png", get(move || async move { served }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let mut state = test_state(&pool).await;
        state.fetcher = crate::fetch::Fetcher::new(std::time::Duration::from_secs(10), true);
        let app = TestApp::new(state.clone(), routes(max_bytes(&state)));
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let link = format!("http://{addr}/art.png");

        let fields = vec![("url", link.clone()), ("rating", "g".to_owned())];
        let response = app
            .post_multipart("/upload", Some(&session), &fields, None)
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let id = response
            .location
            .unwrap()
            .strip_prefix("/posts/")
            .unwrap()
            .parse()
            .unwrap();
        let post = posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.source, link);
        let asset = media::for_post(&pool, id).await.unwrap().unwrap();
        assert_eq!(asset.sha256, Sha256::digest(&png).to_vec());
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn links_to_the_local_network_are_refused(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        for link in [
            "http://127.0.0.1:5432/",
            "http://169.254.169.254/latest/meta-data/",
            "file:///etc/passwd",
        ] {
            let fields = vec![("url", link.to_owned()), ("rating", "g".to_owned())];
            let response = app
                .post_multipart("/upload", Some(&session), &fields, None)
                .await;
            assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{link}");
            // Kept in the form (templates escape "/" as "&#x2f;").
            let escaped = link.replace('/', "&#x2f;");
            assert!(
                response.body.contains(&format!("value=\"{escaped}\"")),
                "link kept in the form"
            );
        }
    }

    fn fields(rating: &str) -> Vec<(&'static str, String)> {
        vec![
            ("rating", rating.to_owned()),
            ("source", "https://example.com/art".to_owned()),
            ("description", "a test pattern".to_owned()),
        ]
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn uploads_create_a_post_asset_and_job(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(320, 240);

        let response = app
            .post_multipart(
                "/upload",
                Some(&session),
                &fields("q"),
                Some(("art.png", &png)),
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let post_id: i64 = response
            .location
            .unwrap()
            .strip_prefix("/posts/")
            .unwrap()
            .parse()
            .unwrap();

        let post = posts::by_id(&pool, post_id).await.unwrap().unwrap();
        assert_eq!(
            (post.rating, post.status),
            (Rating::Questionable, PostStatus::Active)
        );
        assert_eq!(post.source, "https://example.com/art");
        let asset = media::for_post(&pool, post_id).await.unwrap().unwrap();
        assert_eq!(
            (asset.media_type.as_str(), asset.width, asset.height),
            ("png", 320, 240)
        );
        assert_eq!(asset.sha256, Sha256::digest(&png).to_vec());
        assert_eq!(asset.md5, Md5::digest(&png).to_vec());
        assert!(
            state
                .storage
                .exists(&Key::parse(&asset.storage_key).unwrap())
                .await
                .unwrap()
        );

        let job = jobs::claim(&pool, "test", std::time::Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (job.kind.as_str(), job.payload["asset_id"].as_i64()),
            ("media.process", Some(asset.id))
        );
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn duplicates_point_to_the_existing_post(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(64, 64);
        let first = app
            .post_multipart(
                "/upload",
                Some(&session),
                &fields("g"),
                Some(("a.png", &png)),
            )
            .await;
        let existing = first.location.unwrap();

        let again = app
            .post_multipart(
                "/upload",
                Some(&session),
                &fields("g"),
                Some(("b.png", &png)),
            )
            .await;
        assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            again.body.contains(&format!("href=\"{existing}\"")),
            "{}",
            again.body
        );
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn bad_files_and_fields_are_explained_and_kept(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;

        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec();
        let response = app
            .post_multipart(
                "/upload",
                Some(&session),
                &fields("g"),
                Some(("x.svg", &svg)),
            )
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            response
                .body
                .contains("This file type isn&#x27;t supported"),
            "{}",
            response.body
        );
        assert!(response.body.contains("a test pattern"), "fields are kept");

        let png = fixture::png(16, 16);
        let response = app
            .post_multipart(
                "/upload",
                Some(&session),
                &fields("nope"),
                Some(("a.png", &png)),
            )
            .await;
        assert!(
            response.body.contains("Choose a rating."),
            "{}",
            response.body
        );

        let response = app
            .post_multipart("/upload", Some(&session), &fields("g"), None)
            .await;
        assert!(
            response
                .body
                .contains("Choose a file to upload, or paste a link."),
            "{}",
            response.body
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM posts")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn tags_are_created_and_bad_ones_explained(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(16, 16);
        let mut form = fields("g");
        form.push(("tags", "Long_Hair -solo artist:someone".to_owned()));
        let response = app
            .post_multipart("/upload", Some(&session), &form, Some(("a.png", &png)))
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            response.body.contains("`-solo` may not start with `-`"),
            "{}",
            response.body
        );
        assert!(response.body.contains("Long_Hair -solo artist:someone"));

        form.last_mut().unwrap().1 = "Long_Hair artist:someone".to_owned();
        let response = app
            .post_multipart("/upload", Some(&session), &form, Some(("a.png", &png)))
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let id: i64 = response.location.unwrap()["/posts/".len()..]
            .parse()
            .unwrap();
        let post = posts::by_id(&pool, id).await.unwrap().unwrap();
        let tags = uwuu_db::tags::by_ids(&pool, &post.tag_ids).await.unwrap();
        let mut summary: Vec<(String, i16, i32)> = tags
            .into_iter()
            .map(|t| (t.name, t.category_id, t.post_count))
            .collect();
        summary.sort();
        assert_eq!(
            summary,
            [("long_hair".into(), 0, 1), ("someone".into(), 1, 1)]
        );
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn oversized_files_are_refused(pool: PgPool) {
        let mut config = crate::test_support::test_config();
        config.media.max_upload_mb = 1;
        let state = crate::test_support::test_state_with(&pool, config).await;
        let app = TestApp::new(state.clone(), routes(max_bytes(&state)));
        let session = session_for(&pool, "alice", SystemRole::Member).await;

        let big = vec![0u8; 1024 * 1024 + 10];
        let response = app
            .post_multipart(
                "/upload",
                Some(&session),
                &fields("g"),
                Some(("big.png", &big)),
            )
            .await;
        assert_eq!(
            response.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            response.body
        );
        assert!(
            response.body.contains("larger than 1 MB"),
            "{}",
            response.body
        );
        // The partial temp file was cleaned up.
        let leftovers = std::fs::read_dir(&state.work_dir).unwrap().count();
        assert_eq!(leftovers, 0);
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn approval_queue_applies_to_roles_without_the_bypass(pool: PgPool) {
        settings::set(&pool, "upload_approval", json!(true))
            .await
            .unwrap();
        let (app, _) = app(&pool).await;
        let member = session_for(&pool, "alice", SystemRole::Member).await;
        let contributor = session_for(&pool, "bob", SystemRole::Contributor).await;

        let queued = app
            .post_multipart(
                "/upload",
                Some(&member),
                &fields("g"),
                Some(("a.png", &fixture::png(20, 20))),
            )
            .await;
        let direct = app
            .post_multipart(
                "/upload",
                Some(&contributor),
                &fields("g"),
                Some(("b.png", &fixture::png(30, 30))),
            )
            .await;
        let status_of = async |location: Option<String>| {
            let id = location
                .unwrap()
                .strip_prefix("/posts/")
                .unwrap()
                .parse()
                .unwrap();
            posts::by_id(&pool, id).await.unwrap().unwrap().status
        };
        assert_eq!(status_of(queued.location).await, PostStatus::Pending);
        assert_eq!(status_of(direct.location).await, PostStatus::Active);
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn visitors_must_log_in_to_upload(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let response = app.get("/upload", None).await;
        assert_eq!(response.location.as_deref(), Some("/login?next=%2Fupload"));
        let response = app
            .post_multipart("/upload", None, &fields("g"), None)
            .await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }
}
