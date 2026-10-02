//! Uploading posts.
//!
//! The file is streamed to a temporary file while it is hashed and
//! measured, then [`prepare`] identifies and probes it and stores the
//! original under its content hash, and [`create_post`] creates the post,
//! its media record and the processing job in one transaction.
//!
//! People upload in two steps (see [`crate::uploads`]): files are staged
//! first, then each is posted from its own form. `POST /upload` still
//! takes a whole post in one form, for scripts, through [`ingest`].

use std::path::{Path, PathBuf};

use axum::Router;
use axum::extract::multipart::{Field, MultipartError};
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use md5::Md5;
use moekura_core::jobs::ProcessMedia;
use moekura_core::permissions::Permission;
use moekura_core::post_edit::Metatag;
use moekura_core::posts::{DESCRIPTION_MAX_LEN, PostStatus, Rating, SOURCE_MAX_LEN};
use moekura_core::uploads::{self, UploadLimits};
use moekura_db::media::{self, InsertAssetError, NewAsset};
use moekura_db::posts::{self, NewPost};
use moekura_db::staged_uploads;
use moekura_media::MediaError;
use moekura_storage::Key;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::edit::Refused;
use crate::error::AppError;
use crate::pages::Page;

/// Room for the text fields and multipart framing on top of the file.
const FORM_OVERHEAD: usize = 64 * 1024;

/// The request body limit for an upload form of up to `max_upload_bytes`.
pub(crate) fn body_limit(max_upload_bytes: u64) -> DefaultBodyLimit {
    let limit = usize::try_from(max_upload_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(FORM_OVERHEAD);
    DefaultBodyLimit::max(limit)
}

pub fn routes(max_upload_bytes: u64) -> Router<AppState> {
    Router::new().route(
        "/upload",
        get(|| async { Redirect::permanent("/uploads/new") })
            .post(upload)
            .layer(body_limit(max_upload_bytes)),
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
    /// The parent post's number, as typed (a `parent:` metatag wins).
    pub parent: String,
    pub description: String,
    /// The artist's commentary, as given; when both are empty, the
    /// source's (if it has one) is used.
    pub commentary_title: String,
    pub commentary_description: String,
    /// Upload even if posts look like the file (the uploader saw the
    /// warning).
    pub allow_similar: bool,
    /// A file the uploader sent before and was warned about, to post
    /// instead of a new one.
    pub staged: Option<i64>,
}

/// An uploaded file on local disk, removed when dropped.
pub struct TempUpload {
    path: PathBuf,
    pub sha256: [u8; 32],
    pub md5: [u8; 16],
    pub size: u64,
    /// The file's name as sent, or the link it came from.
    name: String,
}

impl TempUpload {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: &str) {
        // As much as the database keeps.
        self.name = name.chars().take(SOURCE_MAX_LEN).collect();
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
    /// Posts look like the file; it waits as a staged upload until the
    /// uploader confirms.
    #[error("this file looks like posts already here")]
    Similar(Lookalikes),
    /// Over the uploader's upload limits.
    #[error("{0}")]
    Limit(String),
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

/// Posts an upload looks like, and where the file waits meanwhile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookalikes {
    /// Closest first, only those the uploader may see.
    pub posts: Vec<i64>,
    /// The staged upload holding the file: confirming posts it.
    pub staged: i64,
    /// The upload it's in.
    pub upload: i64,
}

/// Most look-alikes an upload warns about.
const LOOKALIKES_SHOWN: usize = 8;

/// What `uploader` may still upload now.
pub(crate) struct Allowance {
    /// Why they can't upload now, if they can't.
    pub refusal: Option<String>,
    /// Uploads left before the approval queue's limit, if one applies.
    pub pending_left: Option<i64>,
    /// Uploads left today, if there's a daily limit.
    pub today_left: Option<i64>,
}

/// Works out `uploader`'s [`Allowance`] from their role's limits.
pub(crate) async fn allowance(
    state: &AppState,
    uploader: &CurrentUser,
) -> Result<Allowance, sqlx::Error> {
    let limits = uploader.role.upload_limits;
    let Some(user) = &uploader.user else {
        return Ok(Allowance {
            refusal: None,
            pending_left: None,
            today_left: None,
        });
    };
    if limits == UploadLimits::default() {
        return Ok(Allowance {
            refusal: None,
            pending_left: None,
            today_left: None,
        });
    }
    let settings = &state.site.get().settings;
    let counts = posts::upload_counts(state.db.primary(), user.id).await?;
    let queued = settings.upload_approval && !uploader.can(Permission::UploadWithoutApproval);
    let scaling = settings.upload_limit_scaling;
    Ok(Allowance {
        refusal: uploads::refusal(&limits, &counts, scaling, queued),
        pending_left: limits
            .pending
            .filter(|_| queued)
            .map(|base| (uploads::pending_limit(base, &counts, scaling) - counts.pending).max(0)),
        today_left: limits
            .daily
            .map(|daily| (i64::from(daily) - counts.today).max(0)),
    })
}

/// Refuses an upload over `uploader`'s limits.
pub(crate) async fn check_limits(
    state: &AppState,
    uploader: &CurrentUser,
) -> Result<(), UploadError> {
    match allowance(state, uploader).await?.refusal {
        Some(message) => Err(UploadError::Limit(message)),
        None => Ok(()),
    }
}

fn max_bytes(state: &AppState) -> u64 {
    state.media.config().max_upload_mb * 1024 * 1024
}

/// The upload form again, with what went wrong.
fn render_form(
    page: &Page,
    fields: &UploadFields,
    error: &UploadError,
    status: StatusCode,
) -> Response {
    crate::uploads::new_form(page, &fields.url, Some(error), status, None)
}

async fn upload(
    State(state): State<AppState>,
    page: Page,
    multipart: Multipart,
) -> Result<Response, AppError> {
    page.current.require(Permission::Upload)?;
    if let Err(error) = check_limits(&state, &page.current).await {
        return Ok(failed(&page, &UploadFields::default(), error));
    }
    let (mut fields, file) = match receive(&state, multipart).await {
        (fields, Ok(file)) => (fields, file),
        (fields, Err(error)) => return Ok(failed(&page, &fields, error)),
    };
    // A new file, else the one waiting from a warning, else a link.
    let posted = match (file, fields.staged) {
        (Some(file), _) => ingest(&state, &page.current, &file, &fields, true).await,
        (None, Some(staged)) => post_staged(&state, &page.current, staged, &fields).await,
        (None, None) if !fields.url.is_empty() => match fetch_url(&state, &mut fields).await {
            Ok(file) => ingest(&state, &page.current, &file, &fields, true).await,
            Err(error) => Err(error),
        },
        (None, None) => Err(UploadError::Invalid(
            "Choose a file to upload, or paste a link.".into(),
        )),
    };
    match posted {
        Ok(post_id) => {
            let kept =
                crate::tag_warnings::kept_categories(state.db.primary(), &fields.tags, post_id)
                    .await?;
            let query = crate::tag_warnings::check_query(&kept);
            Ok(Redirect::to(&format!("/posts/{post_id}?{query}")).into_response())
        }
        // The file waits in an upload, where the look-alikes are shown
        // beside its form.
        Err(UploadError::Similar(found)) => {
            Ok(Redirect::to(&format!("/uploads/{}", found.upload)).into_response())
        }
        Err(error) => Ok(failed(&page, &fields, error)),
    }
}

/// Downloads `fields.url`, which also becomes the source if none was
/// given. A work's page on a site the source strategies know (or any page
/// naming its image) downloads the work's best file instead, and the page
/// becomes the source.
pub(crate) async fn fetch_url(
    state: &AppState,
    fields: &mut UploadFields,
) -> Result<TempUpload, UploadError> {
    let found = state.sources.lookup(&fields.url).await;
    let (file_url, page) = match found.as_deref() {
        Some(info) if !info.files.is_empty() => (info.files[0].clone(), info.page_url.clone()),
        _ => (fields.url.clone(), fields.url.clone()),
    };
    let file = download(state, &file_url, found.as_deref()).await?;
    if fields.source.is_empty() {
        fields.source = page;
    }
    Ok(file)
}

/// Downloads `file_url`, one of the files of `info` if it came from a
/// source's page. A Pixiv ugoira's zip lacks its frames' delays; they're
/// added to it.
pub(crate) async fn download(
    state: &AppState,
    file_url: &str,
    info: Option<&crate::sources::SourceInfo>,
) -> Result<TempUpload, UploadError> {
    let url = url::Url::parse(file_url).map_err(|_| match info {
        Some(info) => {
            UploadError::Invalid(format!("{} gave a file link that isn't valid.", info.site))
        }
        None => UploadError::Invalid("That isn't a valid link.".into()),
    })?;
    let headers = info.map(|i| i.header_pairs()).unwrap_or_default();
    let writer = TempWriter::create(&state.work_dir).await?;
    let mut file = state
        .fetcher
        .fetch_with(&url, &headers, writer, max_bytes(state))
        .await?;
    file.set_name(file_url);
    let frames = info
        .filter(|i| i.files.first().is_some_and(|f| f == file_url))
        .and_then(|i| i.ugoira_frames.as_ref());
    if let Some(frames) = frames {
        let frames: Vec<moekura_media::ugoira::Frame> = frames
            .iter()
            .map(|(file, delay_ms)| moekura_media::ugoira::Frame {
                file: file.clone(),
                delay_ms: *delay_ms,
            })
            .collect();
        let path = file.path.clone();
        tokio::task::spawn_blocking(move || moekura_media::ugoira::add_frame_data(&path, &frames))
            .await
            .map_err(|e| UploadError::Internal(e.to_string()))?
            .map_err(|e| UploadError::Invalid(format!("The ugoira's zip is damaged ({e}).")))?;
        file.rehash().await?;
    }
    Ok(file)
}

/// Saves the upload's commentary on post `post_id`: the one given, or
/// else the source's. Only logged if that fails; the post is made.
async fn save_commentary(
    state: &AppState,
    uploader: &CurrentUser,
    post_id: i64,
    fields: &UploadFields,
) {
    let mut texts = moekura_db::artist_commentaries::Texts {
        original_title: fields.commentary_title.clone(),
        original_description: fields.commentary_description.clone(),
        ..Default::default()
    };
    if texts.is_empty()
        && !fields.source.is_empty()
        && let Some(info) = state.sources.lookup(&fields.source).await
    {
        texts.original_title = info.title.clone();
        texts.original_description = info.description.clone();
    }
    if texts.is_empty() {
        return;
    }
    let saved = match crate::commentary::clean(texts) {
        Ok(texts) => moekura_db::artist_commentaries::save(
            state.db.primary(),
            post_id,
            &texts,
            uploader.user.as_ref().map(|u| u.id),
            None,
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string()),
        Err(error) => Err(error.public_message().to_owned()),
    };
    if let Err(error) = saved {
        tracing::warn!(post_id, error, "commentary from the upload not saved");
    }
}

fn failed(page: &Page, fields: &UploadFields, error: UploadError) -> Response {
    if let UploadError::Internal(detail) = &error {
        tracing::error!(error = %detail, "upload failed");
        let error =
            UploadError::Internal("Something went wrong on our side. Please try again.".into());
        return render_form(page, fields, &error, StatusCode::INTERNAL_SERVER_ERROR);
    }
    render_form(page, fields, &error, error_status(&error))
}

/// The status a page about `error` has.
pub(crate) fn error_status(error: &UploadError) -> StatusCode {
    match error {
        UploadError::Limit(_) => StatusCode::TOO_MANY_REQUESTS,
        UploadError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

/// Reads the form, streaming the file to disk. The fields read so far come
/// back even on error, so the form can be shown again filled in.
pub(crate) async fn receive(
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
            "url"
            | "rating"
            | "staged"
            | "allow_similar"
            | "tags"
            | "source"
            | "parent"
            | "description"
            | "commentary_title"
            | "commentary_description" => {
                let text = match field.text().await {
                    Ok(text) => text,
                    Err(error) => return (fields, Err(multipart_error(state, &error))),
                };
                match name.as_str() {
                    "url" => fields.url = text.trim().to_owned(),
                    "staged" => fields.staged = text.trim().parse().ok(),
                    "allow_similar" => {
                        fields.allow_similar = matches!(text.trim(), "1" | "true" | "on");
                    }
                    "rating" => fields.rating = text.parse().ok(),
                    "tags" => fields.tags = text,
                    "source" => fields.source = text.trim().to_owned(),
                    "parent" => fields.parent = text.trim().to_owned(),
                    "commentary_title" => fields.commentary_title = text.trim().to_owned(),
                    "commentary_description" => {
                        fields.commentary_description = text.trim().to_owned();
                    }
                    _ => fields.description = text.trim().to_owned(),
                }
            }
            _ => {}
        }
    }
    (fields, Ok(file))
}

pub(crate) fn multipart_error(state: &AppState, error: &MultipartError) -> UploadError {
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

pub(crate) async fn save_to_temp(
    state: &AppState,
    mut field: Field<'_>,
) -> Result<TempUpload, UploadError> {
    let limit = max_bytes(state);
    let name = field.file_name().unwrap_or_default().to_owned();
    let mut writer = TempWriter::create(&state.work_dir).await?;
    writer.upload.set_name(&name);
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

impl TempUpload {
    /// Recomputes the hashes and size after the file was changed.
    async fn rehash(&mut self) -> Result<(), UploadError> {
        use tokio::io::AsyncReadExt;
        let failed = |e: std::io::Error| UploadError::Internal(format!("reading temp file: {e}"));
        let mut file = tokio::fs::File::open(&self.path).await.map_err(failed)?;
        let (mut sha256, mut md5, mut size) = (Sha256::new(), Md5::new(), 0u64);
        let mut buffer = vec![0; 256 * 1024];
        loop {
            let read = file.read(&mut buffer).await.map_err(failed)?;
            if read == 0 {
                break;
            }
            sha256.update(&buffer[..read]);
            md5.update(&buffer[..read]);
            size += read as u64;
        }
        self.sha256 = sha256.finalize().into();
        self.md5 = md5.finalize().into();
        self.size = size;
        Ok(())
    }
}

impl TempWriter {
    pub async fn create(dir: &Path) -> Result<Self, UploadError> {
        let name = hex::encode(moekura_core::tokens::NewToken::generate().hash);
        let upload = TempUpload {
            path: dir.join(format!("upload-{}", &name[..24])),
            sha256: [0; 32],
            md5: [0; 16],
            size: 0,
            name: String::new(),
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

/// A file checked and stored, ready to become a post: what [`prepare`]
/// found, and what [`create_post`] needs. Staged uploads keep it in the
/// database between the two.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub sha256: [u8; 32],
    pub md5: [u8; 16],
    /// `media_assets.media_type`.
    pub media_type: String,
    pub width: i32,
    pub height: i32,
    pub duration_ms: Option<i32>,
    pub frames: i32,
    pub has_audio: bool,
    pub file_size: i64,
    /// Where the original is stored.
    pub storage_key: String,
}

/// The rating, source and parent an upload gets: the fields, unless
/// metatags in the tags say otherwise.
struct Chosen {
    rating: Rating,
    source: String,
    parent_id: Option<i64>,
}

/// Checks the fields a post needs, before any work on the file.
fn check_fields(fields: &UploadFields, metatags: &[Metatag]) -> Result<Chosen, UploadError> {
    let mut rating = fields.rating;
    let mut source = fields.source.clone();
    let mut parent_id = match fields.parent.trim().trim_start_matches('#') {
        "" => None,
        number => Some(
            number
                .parse::<i64>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| UploadError::Invalid("The parent must be a post number.".into()))?,
        ),
    };
    for metatag in metatags {
        match metatag {
            Metatag::Rating(r) => rating = Some(*r),
            Metatag::Source(s) => source.clone_from(s),
            Metatag::Parent(p) => parent_id = *p,
            _ => {}
        }
    }
    let rating = rating.ok_or_else(|| UploadError::Invalid("Choose a rating.".into()))?;
    if source.chars().count() > SOURCE_MAX_LEN {
        return Err(UploadError::Invalid(format!(
            "The source may be at most {SOURCE_MAX_LEN} characters."
        )));
    }
    if fields.description.chars().count() > DESCRIPTION_MAX_LEN {
        return Err(UploadError::Invalid(format!(
            "The description may be at most {DESCRIPTION_MAX_LEN} characters."
        )));
    }
    Ok(Chosen {
        rating,
        source,
        parent_id,
    })
}

impl From<Refused> for UploadError {
    fn from(error: Refused) -> Self {
        match error {
            Refused::Invalid(message) => Self::Invalid(message),
            Refused::Error(AppError::Internal(detail)) => Self::Internal(detail),
            Refused::Error(error) => Self::Invalid(error.public_message().to_owned()),
        }
    }
}

/// Turns a received file into a post. Returns the new post's id.
///
/// With `warn_similar`, unless `fields.allow_similar` is set, a file that
/// looks like posts the uploader can see isn't posted: it's kept as a
/// staged upload and [`UploadError::Similar`] names the posts, so the
/// uploader can look and then confirm with [`post_staged`].
pub async fn ingest(
    state: &AppState,
    uploader: &CurrentUser,
    file: &TempUpload,
    fields: &UploadFields,
    warn_similar: bool,
) -> Result<i64, UploadError> {
    // Bad tags and fields are refused before the file is looked at.
    let tags = crate::tags::parse_field(state.db.primary(), &fields.tags).await?;
    check_fields(fields, &tags.metatags)?;
    crate::metatags::prepare(state, uploader, None, &tags.metatags).await?;
    let prepared = prepare(state, file).await?;
    // Staging needs an account to hold the file for.
    if warn_similar
        && !fields.allow_similar
        && let Some(user) = &uploader.user
    {
        let hash = phash(state, file).await;
        let posts = lookalikes(state, uploader, hash, None).await?;
        if !posts.is_empty() {
            let (upload, staged) =
                stage(state, user.id, file, &prepared, hash, &fields.source).await?;
            return Err(UploadError::Similar(Lookalikes {
                posts,
                staged,
                upload,
            }));
        }
    }
    create_post(state, uploader, &prepared, fields).await
}

/// The perceptual hash of `file`, as processing would make it, if it can
/// be hashed.
pub(crate) async fn phash(state: &AppState, file: &TempUpload) -> Option<u64> {
    match crate::image_search::hash_file(state, file).await {
        Ok(hash) => Some(hash),
        Err(error) => {
            tracing::debug!(
                error = error.public_message(),
                "upload not checked for similar posts"
            );
            None
        }
    }
}

/// Posts that look like a file with perceptual hash `hash` (within the
/// distance the index always finds) which `uploader` may see and hasn't
/// blacklisted, closest first, leaving out `except`. Files that can't be
/// hashed have none.
pub(crate) async fn lookalikes(
    state: &AppState,
    uploader: &CurrentUser,
    hash: Option<u64>,
    except: Option<i64>,
) -> Result<Vec<i64>, UploadError> {
    let Some(hash) = hash.filter(|_| uploader.can(Permission::ViewPosts)) else {
        return Ok(Vec::new());
    };
    let db = state.db.primary();
    let found = media::similar(
        db,
        hash,
        media::SIMILAR_MAX_DISTANCE,
        except,
        (LOOKALIKES_SHOWN * 3) as i64,
    )
    .await?;
    let ids: Vec<i64> = found.iter().map(|s| s.post_id).collect();
    let candidates = posts::by_ids(db, &ids).await?;
    let visible = crate::posts::visibility(uploader);
    let blacklist = crate::blacklist::for_viewer(state, db, uploader).await?;
    Ok(found
        .iter()
        .filter_map(|s| candidates.iter().find(|p| p.id == s.post_id))
        .filter(|p| {
            visible.allows(p)
                && blacklist
                    .as_ref()
                    .is_none_or(|list| list.matching(p.rating, &p.tag_ids).is_none())
        })
        .map(|p| p.id)
        .take(LOOKALIKES_SHOWN)
        .collect())
}

/// Keeps a prepared file for `uploader_id` to post later, as an upload of
/// its own. Returns the upload's id and the staged file's. Unused staged
/// uploads expire with their files.
pub(crate) async fn stage(
    state: &AppState,
    uploader_id: i64,
    file: &TempUpload,
    prepared: &Prepared,
    hash: Option<u64>,
    source: &str,
) -> Result<(i64, i64), UploadError> {
    let mut tx = state.db.primary().begin().await?;
    let upload = staged_uploads::create_upload(&mut *tx, uploader_id, source).await?;
    let slot = staged_uploads::Slot {
        upload_id: upload,
        uploader_id,
        position: 0,
        file_name: file.name(),
        source,
    };
    let staged = staged_uploads::create(&mut *tx, slot, prepared.stored(hash)).await?;
    crate::suggestions::queue_staged(state, &mut tx, staged).await?;
    tx.commit().await?;
    Ok((upload, staged))
}

/// Posts staged upload `id`, which must be `uploader`'s and not posted
/// yet, with `fields`. Returns the new post's id.
pub async fn post_staged(
    state: &AppState,
    uploader: &CurrentUser,
    id: i64,
    fields: &UploadFields,
) -> Result<i64, UploadError> {
    let db = state.db.primary();
    let gone =
        || UploadError::Invalid("The file you sent before has expired; send it again.".into());
    let user = uploader.user.as_ref().ok_or_else(gone)?;
    let staged = staged_uploads::by_id(db, id)
        .await?
        .filter(|s| s.uploader_id == user.id)
        .ok_or_else(gone)?;
    if let Some(post) = staged.post_id {
        return Err(UploadError::Duplicate(post));
    }
    let prepared = Prepared::from_staged(&staged).ok_or_else(gone)?;
    let post_id = create_post(state, uploader, &prepared, fields).await?;
    staged_uploads::used(db, id, post_id).await?;
    Ok(post_id)
}

/// Checks a received file isn't a duplicate, identifies and probes it,
/// and stores the original (without its metadata, if the site strips
/// it).
pub async fn prepare(state: &AppState, file: &TempUpload) -> Result<Prepared, UploadError> {
    let db = state.db.primary();
    if let Some(existing) = media::post_with_sha256(db, &file.sha256).await? {
        return Err(UploadError::Duplicate(existing));
    }
    let media_type = state
        .media
        .identify(file.path())
        .await
        .map_err(media_error)?;
    // The stripped file is what's kept, so its hashes are the post's; a
    // file whose stripped bytes are a post's is that post's too.
    let stripped = strip_metadata(state, file, media_type).await?;
    let file = match &stripped {
        Some(stripped) => {
            if let Some(existing) = media::post_with_sha256(db, &stripped.sha256).await? {
                return Err(UploadError::Duplicate(existing));
            }
            stripped
        }
        None => file,
    };
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
    let as_i32 = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
    Ok(Prepared {
        sha256: file.sha256,
        md5: file.md5,
        media_type: media_type.name().to_owned(),
        width: as_i32(probe.width),
        height: as_i32(probe.height),
        duration_ms: probe.duration_ms.map(as_i32),
        frames: as_i32(probe.frames),
        has_audio: probe.has_audio,
        file_size: i64::try_from(file.size).unwrap_or(i64::MAX),
        storage_key: key.as_str().to_owned(),
    })
}

/// `file` without its metadata, when `media.strip_metadata` asks for it
/// and there was some to remove. `require` refuses types it can't be
/// removed from.
async fn strip_metadata(
    state: &AppState,
    file: &TempUpload,
    media_type: moekura_media::MediaType,
) -> Result<Option<TempUpload>, UploadError> {
    use moekura_core::config::StripMetadata;
    let setting = state.media.config().strip_metadata;
    if setting == StripMetadata::Off {
        return Ok(None);
    }
    if !moekura_media::strip::supports(media_type) {
        if setting == StripMetadata::Require {
            return Err(UploadError::Invalid(format!(
                "This site removes identifying metadata from uploads, which it can't do for \
                 {} files; JPEG, PNG and WebP files are accepted.",
                media_type.name().to_uppercase()
            )));
        }
        tracing::info!(
            media_type = media_type.name(),
            "metadata not stripped: unsupported type"
        );
        return Ok(None);
    }
    let path = file.path.with_extension("stripped");
    // Removed when dropped, should hashing fail.
    let mut stripped = TempUpload {
        path,
        sha256: [0; 32],
        md5: [0; 16],
        size: 0,
        name: file.name.clone(),
    };
    match state
        .media
        .strip_metadata(file.path(), media_type, &stripped.path)
        .await
        .map_err(media_error)?
    {
        moekura_media::strip::Stripped::Changed => {
            stripped.rehash().await?;
            Ok(Some(stripped))
        }
        _ => Ok(None),
    }
}

impl Prepared {
    /// The file a staged upload keeps, once it's there.
    pub fn from_staged(staged: &staged_uploads::Staged) -> Option<Self> {
        Some(Self {
            sha256: staged.sha256.as_deref()?.try_into().ok()?,
            md5: staged.md5.as_deref()?.try_into().ok()?,
            media_type: staged.media_type.clone()?,
            width: staged.width?,
            height: staged.height?,
            duration_ms: staged.duration_ms,
            frames: staged.frames?,
            has_audio: staged.has_audio?,
            file_size: staged.file_size?,
            storage_key: staged.storage_key.clone()?,
        })
    }

    /// What a staged upload keeps of the file, with its perceptual hash.
    pub fn stored(&self, phash: Option<u64>) -> staged_uploads::StoredFile<'_> {
        staged_uploads::StoredFile {
            sha256: &self.sha256,
            md5: &self.md5,
            media_type: &self.media_type,
            width: self.width,
            height: self.height,
            duration_ms: self.duration_ms,
            frames: self.frames,
            has_audio: self.has_audio,
            file_size: self.file_size,
            storage_key: &self.storage_key,
            // Stored as Postgres stores it: the same 64 bits, signed.
            phash: phash.map(|h| h as i64),
        }
    }
}

/// Makes a post of a prepared file. Returns the new post's id.
pub async fn create_post(
    state: &AppState,
    uploader: &CurrentUser,
    prepared: &Prepared,
    fields: &UploadFields,
) -> Result<i64, UploadError> {
    let db = state.db.primary();
    let tags = crate::tags::parse_field(db, &fields.tags).await?;
    let Chosen {
        rating,
        source,
        parent_id,
    } = check_fields(fields, &tags.metatags)?;
    let effects = crate::metatags::prepare(state, uploader, None, &tags.metatags).await?;
    if let Some(parent) = parent_id {
        let visible = posts::by_id(db, parent)
            .await?
            .is_some_and(|p| crate::posts::visibility(uploader).allows(&p));
        if !visible {
            return Err(UploadError::Invalid(format!("There is no post #{parent}.")));
        }
    }
    let site = state.site.get();
    let status =
        if site.settings.upload_approval && !uploader.can(Permission::UploadWithoutApproval) {
            PostStatus::Pending
        } else {
            PostStatus::Active
        };

    let mut tx = db.begin().await?;
    // Credits the tags this creates; the post is the uploader's anyway.
    moekura_db::post_versions::attribute(&mut tx, uploader.user.as_ref().map(|u| u.id), None)
        .await?;
    let found = moekura_db::tags::for_post(
        &mut tx,
        &tags.wanted(),
        uploader.can(Permission::ManageTags),
    )
    .await?;
    let tag_ids: Vec<i32> =
        crate::tag_warnings::with_request_tags(&mut tx, site.settings.request_tags, found)
            .await?
            .iter()
            .map(|t| t.id)
            .collect();
    crate::artists::refuse_banned(state, &mut *tx, uploader, &tag_ids, &[])
        .await
        .map_err(UploadError::Invalid)?;
    let post_id = posts::insert(
        &mut *tx,
        NewPost {
            uploader_id: uploader.user.as_ref().map(|u| u.id),
            rating,
            status,
            source: &source,
            description: &fields.description,
            tag_ids: &tag_ids,
        },
    )
    .await?;
    let asset = NewAsset {
        post_id,
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
    };
    let asset_id = match media::insert(&mut *tx, asset).await {
        Ok(id) => id,
        // Someone uploaded the same file a moment ago.
        Err(InsertAssetError::Duplicate(_)) => {
            drop(tx);
            let existing = media::post_with_sha256(db, &prepared.sha256)
                .await?
                .unwrap_or_default();
            return Err(UploadError::Duplicate(existing));
        }
        Err(InsertAssetError::Db(error)) => return Err(error.into()),
    };
    moekura_db::jobs::enqueue(&mut tx, &ProcessMedia { asset_id }).await?;
    tx.commit().await?;

    tracing::info!(
        post_id,
        media_type = prepared.media_type,
        size = prepared.file_size,
        ?status,
        "post uploaded"
    );
    // The post is made; what can't be done now is only logged.
    let mut applied = Ok(());
    if parent_id.is_some() {
        applied = crate::edit::set_parent(state, uploader, post_id, parent_id).await;
    }
    if applied.is_ok() {
        applied = crate::metatags::apply(state, uploader, post_id, &effects).await;
    }
    if let Err(error) = applied {
        let message = UploadError::from(error).to_string();
        tracing::warn!(post_id, message, "metatags from the upload not applied");
    }
    save_commentary(state, uploader, post_id, fields).await;
    crate::webhooks::emit_post(
        state,
        moekura_core::webhooks::Event::PostCreated,
        post_id,
        serde_json::json!({}),
    )
    .await;
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
    use moekura_core::permissions::SystemRole;
    use moekura_db::{jobs, settings};
    use serde_json::json;
    use sqlx::PgPool;

    use axum::routing::get;

    use super::*;
    use crate::test_support::{TestApp, fixture, session_for, test_state};

    async fn app(pool: &PgPool) -> (TestApp, AppState) {
        let state = test_state(pool).await;
        let routes = routes(max_bytes(&state))
            .merge(crate::uploads::routes(max_bytes(&state)))
            .merge(crate::posts::routes());
        (TestApp::new(state.clone(), routes), state)
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let post = posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.source, link);
        let asset = media::for_post(&pool, id).await.unwrap().unwrap();
        assert_eq!(asset.sha256, Sha256::digest(&png).to_vec());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn ugoira_zips(pool: PgPool) {
        use std::io::Write;

        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for n in 0..2 {
            zip.start_file(format!("{n:06}.png"), options).unwrap();
            zip.write_all(&fixture::png(40, 30)).unwrap();
        }
        zip.start_file("animation.json", options).unwrap();
        zip.write_all(
            br#"{"frames":[{"file":"000000.png","delay":50},{"file":"000001.png","delay":70}]}"#,
        )
        .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let fields = vec![("rating", "g".to_owned()), ("tags", "cat".to_owned())];
        let response = app
            .post_multipart("/upload", Some(&session), &fields, Some(("a.zip", &bytes)))
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let id: i64 = response.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let asset = media::for_post(&pool, id).await.unwrap().unwrap();
        assert_eq!(asset.media_type, "ugoira");
        assert_eq!((asset.width, asset.height, asset.frames), (40, 30, 2));
        assert_eq!(asset.duration_ms, Some(120));
        let page = app.get(&format!("/posts/{id}"), Some(&session)).await.body;
        assert!(page.contains("still being prepared"), "{page}");
        let found = app.get("/posts?tags=filetype%3Azip", None).await.body;
        assert!(found.contains(&format!("href=\"/posts/{id}")), "{found}");

        // Other zips aren't posts.
        let mut other = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        other.start_file("notes.txt", options).unwrap();
        other.write_all(b"hi").unwrap();
        let bytes = other.finish().unwrap().into_inner();
        let refused = app
            .post_multipart("/upload", Some(&session), &fields, Some(("b.zip", &bytes)))
            .await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            refused.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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

    /// The id in a `/posts/<id>?…` redirect.
    fn post_in(location: Option<&str>) -> i64 {
        location
            .and_then(|l| l.strip_prefix("/posts/"))
            .and_then(|rest| rest.split('?').next())
            .and_then(|id| id.parse().ok())
            .expect("a redirect to a post")
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn look_alikes_are_shown_before_posting(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let first = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &fields("s"),
                Some(("a.png", &fixture::png(64, 64))),
            )
            .await;
        let original = post_in(first.location.as_deref());
        let again = fixture::png(66, 66);
        crate::test_support::hash_like(&state, &pool, original, &again).await;

        let mut tagged = fields("s");
        tagged.push(("tags", "cat".to_owned()));
        let warned = app
            .post_multipart("/upload", Some(&alice), &tagged, Some(("b.png", &again)))
            .await;
        // The file waits in an upload, whose page shows the look-alikes.
        assert_eq!(warned.status, StatusCode::SEE_OTHER, "{}", warned.body);
        let (upload, staged): (i64, i64) =
            sqlx::query_as("SELECT upload_id, id FROM staged_uploads")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            warned.location.as_deref(),
            Some(format!("/uploads/{upload}").as_str())
        );
        let shown = app.get(&format!("/uploads/{upload}"), Some(&alice)).await;
        assert!(
            shown.body.contains(&format!("href=\"/posts/{original}")),
            "{}",
            shown.body
        );
        assert!(shown.body.contains("Post anyway"), "{}", shown.body);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM posts")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );

        // Confirming posts the kept file, without sending it again; only
        // its uploader can.
        let mut confirm = tagged.clone();
        confirm.push(("staged", staged.to_string()));
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let theirs = app
            .post_multipart("/upload", Some(&bob), &confirm, None)
            .await;
        assert_eq!(theirs.status, StatusCode::UNPROCESSABLE_ENTITY);
        let posted = app
            .post_multipart("/upload", Some(&alice), &confirm, None)
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let post = post_in(posted.location.as_deref());
        let asset = media::for_post(&pool, post).await.unwrap().unwrap();
        assert_eq!(asset.sha256, Sha256::digest(&again).to_vec());
        let twice = app
            .post_multipart("/upload", Some(&alice), &confirm, None)
            .await;
        assert!(
            twice.body.contains(&format!("post #{post}")),
            "{}",
            twice.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn hidden_and_blacklisted_posts_dont_warn(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let mut tagged = fields("s");
        tagged.push(("tags", "dog".to_owned()));
        let first = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &tagged,
                Some(("a.png", &fixture::png(64, 64))),
            )
            .await;
        let original = post_in(first.location.as_deref());
        let again = fixture::png(66, 66);
        crate::test_support::hash_like(&state, &pool, original, &again).await;

        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        sqlx::query("UPDATE users SET settings = '{\"blacklist\": \"dog\"}' WHERE name = 'bob'")
            .execute(&pool)
            .await
            .unwrap();
        let posted = app
            .post_multipart("/upload", Some(&bob), &fields("s"), Some(("b.png", &again)))
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);

        // Deleted posts aren't shown to members either.
        sqlx::query("UPDATE posts SET status = 'deleted'")
            .execute(&pool)
            .await
            .unwrap();
        let carol = session_for(&pool, "carol", SystemRole::Member).await;
        let third = fixture::png(68, 68);
        crate::test_support::hash_like(&state, &pool, original, &third).await;
        let posted = app
            .post_multipart(
                "/upload",
                Some(&carol),
                &fields("s"),
                Some(("c.png", &third)),
            )
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
    }

    fn fields(rating: &str) -> Vec<(&'static str, String)> {
        vec![
            ("rating", rating.to_owned()),
            ("source", "https://example.com/art".to_owned()),
            ("description", "a test pattern".to_owned()),
        ]
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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
            .split('?')
            .next()
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

        let job = jobs::claim(
            &pool,
            "test",
            std::time::Duration::from_secs(60),
            &["media.process"],
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            (job.kind.as_str(), job.payload["asset_id"].as_i64()),
            ("media.process", Some(asset.id))
        );
    }

    /// An app whose `[media] strip_metadata` is `setting`.
    async fn stripping_app(
        pool: &PgPool,
        setting: moekura_core::config::StripMetadata,
    ) -> (TestApp, AppState) {
        let mut config = crate::test_support::test_config();
        config.media.strip_metadata = setting;
        let state = crate::test_support::test_state_with(pool, config).await;
        let routes = routes(max_bytes(&state)).merge(crate::posts::routes());
        (TestApp::new(state.clone(), routes), state)
    }

    async fn stored(state: &AppState, pool: &PgPool, post_id: i64) -> (media::Asset, Vec<u8>) {
        let asset = media::for_post(pool, post_id).await.unwrap().unwrap();
        let path = state.work_dir.join(format!("stored-{post_id}"));
        let key = Key::parse(&asset.storage_key).unwrap();
        state.storage.download(&key, &path).await.unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        (asset, bytes)
    }

    fn post_id(location: Option<String>) -> i64 {
        location
            .and_then(|l| l.strip_prefix("/posts/")?.split('?').next()?.parse().ok())
            .expect("a redirect to the post")
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn originals_can_lose_their_metadata(pool: PgPool) {
        use moekura_core::config::StripMetadata;
        let (app, state) = stripping_app(&pool, StripMetadata::Strip).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png_with_text(64, 48, "my home address");
        let posted = app
            .post_multipart("/upload", Some(&alice), &fields("g"), Some(("a.png", &png)))
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let id = post_id(posted.location);
        let (asset, bytes) = stored(&state, &pool, id).await;
        assert!(!String::from_utf8_lossy(&bytes).contains("my home address"));
        assert!(bytes.len() < png.len());
        // The post's hashes are the stored file's.
        assert_eq!(asset.sha256, Sha256::digest(&bytes).to_vec());
        assert_eq!(asset.md5, Md5::digest(&bytes).to_vec());

        // The same file again, or what was stored, is a duplicate.
        for (name, again) in [("again.png", &png), ("stored.png", &bytes)] {
            let refused = app
                .post_multipart("/upload", Some(&alice), &fields("g"), Some((name, again)))
                .await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{name}");
            assert!(
                refused.body.contains(&format!("post #{id}")),
                "{name}: {}",
                refused.body
            );
        }

        // Types it can't be removed from are kept as they are.
        let gif = fixture::gif(32, 32);
        let posted = app
            .post_multipart("/upload", Some(&alice), &fields("g"), Some(("a.gif", &gif)))
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let (_, bytes) = stored(&state, &pool, post_id(posted.location)).await;
        assert_eq!(bytes, gif);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn metadata_is_kept_unless_asked_and_required_refuses(pool: PgPool) {
        use moekura_core::config::StripMetadata;
        let png = fixture::png_with_text(64, 48, "my home address");
        let (app, state) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let posted = app
            .post_multipart("/upload", Some(&alice), &fields("g"), Some(("a.png", &png)))
            .await;
        let (_, bytes) = stored(&state, &pool, post_id(posted.location)).await;
        assert_eq!(bytes, png, "kept byte for byte by default");

        let (app, _) = stripping_app(&pool, StripMetadata::Require).await;
        let refused = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &fields("g"),
                Some(("a.gif", &fixture::gif(32, 32))),
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("for GIF files"), "{}", refused.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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
        let location = first.location.unwrap();
        let existing = location.split('?').next().unwrap();

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

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn tags_are_created_and_bad_ones_explained(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(16, 16);
        let mut form = fields("g");
        form.push(("tags", "Long_Hair order:score artist:someone".to_owned()));
        let response = app
            .post_multipart("/upload", Some(&session), &form, Some(("a.png", &png)))
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            response
                .body
                .contains("`order:score` may not start with `order:`"),
            "{}",
            response.body
        );

        form.last_mut().unwrap().1 = "Long_Hair artist:someone".to_owned();
        let response = app
            .post_multipart("/upload", Some(&session), &form, Some(("a.png", &png)))
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let id: i64 = response.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let post = posts::by_id(&pool, id).await.unwrap().unwrap();
        let tags = moekura_db::tags::by_ids(&pool, &post.tag_ids)
            .await
            .unwrap();
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

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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
                .split('?')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            posts::by_id(&pool, id).await.unwrap().unwrap().status
        };
        assert_eq!(status_of(queued.location).await, PostStatus::Pending);
        assert_eq!(status_of(direct.location).await, PostStatus::Active);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn upload_limits(pool: PgPool) {
        settings::set(&pool, "upload_approval", json!(true))
            .await
            .unwrap();
        sqlx::query("UPDATE roles SET pending_upload_limit = 1 WHERE system_key = 'member'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE roles SET daily_upload_limit = 1 WHERE system_key = 'contributor'")
            .execute(&pool)
            .await
            .unwrap();
        let (app, _) = app(&pool).await;
        let member = session_for(&pool, "alice", SystemRole::Member).await;
        let contributor = session_for(&pool, "bob", SystemRole::Contributor).await;
        let upload = async |session: &str, width: u32| {
            app.post_multipart(
                "/upload",
                Some(session),
                &fields("g"),
                Some(("a.png", &fixture::png(width, 20))),
            )
            .await
        };

        let form = app.get("/uploads/new", Some(&member)).await;
        assert!(
            form.body
                .contains("You can upload 1 more before some are approved"),
            "{}",
            form.body
        );
        assert_eq!(upload(&member, 20).await.status, StatusCode::SEE_OTHER);
        let refused = upload(&member, 24).await;
        assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            refused.body.contains("waiting for approval"),
            "{}",
            refused.body
        );
        assert!(
            app.get("/uploads/new", Some(&member))
                .await
                .body
                .contains("waiting for approval")
        );

        // Contributors skip the queue, so only their daily limit counts.
        assert_eq!(upload(&contributor, 28).await.status, StatusCode::SEE_OTHER);
        let refused = upload(&contributor, 32).await;
        assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(refused.body.contains("uploads a day"), "{}", refused.body);

        // Approving the member's upload frees their place.
        sqlx::query("UPDATE posts SET status = 'active' WHERE status = 'pending'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(upload(&member, 36).await.status, StatusCode::SEE_OTHER);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn visitors_must_log_in_to_upload(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let response = app.get("/uploads/new", None).await;
        assert_eq!(
            response.location.as_deref(),
            Some("/login?next=%2Fuploads%2Fnew")
        );
        // The old address leads to the form.
        let response = app.get("/upload", None).await;
        assert_eq!(response.location.as_deref(), Some("/uploads/new"));
        let response = app
            .post_multipart("/upload", None, &fields("g"), None)
            .await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }
}
