//! Uploading in two steps, as on Danbooru. `/uploads/new` takes files
//! (several at once) or a link, which become an upload: its files are
//! stored and wait until each is posted from its own form at
//! `/uploads/{id}/assets/{file}`, shown beside a preview, its details and
//! the posts it looks like. A link to a work of several files (a Pixiv
//! gallery) downloads them in the background, and the upload's page
//! shows how far that got. `/uploads` lists the user's files.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::posts::{Rating, SOURCE_MAX_LEN};
use moekura_db::staged_uploads::{self, Slot, Staged, Status, Upload};
use moekura_db::users::User;
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::pages::Page;
use crate::sources::SourceInfo;
use crate::templates::url_value;
use crate::upload::{self, Allowance, TempUpload, UploadError, UploadFields, error_status};

/// Most files sent at once.
pub const MAX_FILES: usize = 20;
/// Most files taken from a link to a work.
const MAX_LINK_FILES: usize = 100;
/// How long creating an upload from a link waits for its files before
/// showing the upload's page, which follows the rest.
const WAIT_FOR_DOWNLOADS: Duration = Duration::from_secs(8);
/// A file still waiting this long after its download started was
/// abandoned (the server restarted meanwhile).
const ABANDONED_AFTER: Duration = Duration::from_secs(10 * 60);
/// Files on a page of `/uploads`.
const PAGE_SIZE: i64 = 48;
/// How often the page of an upload still downloading reloads.
const REFRESH_SECS: u32 = 3;

pub fn routes(max_upload_bytes: u64) -> Router<AppState> {
    let body_limit = upload::body_limit(max_upload_bytes.saturating_mul(MAX_FILES as u64));
    Router::new()
        .route("/uploads", get(index).post(create).layer(body_limit))
        .route("/uploads/new", get(new))
        .route("/uploads/bookmarklet", get(bookmarklet))
        .route("/uploads/{id}", get(show))
        .route("/uploads/{id}/assets/{file}", get(asset).post(post_asset))
        .route(
            "/uploads/{id}/assets/{file}/suggestions",
            get(asset_suggestions),
        )
}

/// The uploading user: uploads are kept for an account.
fn uploader(current: &CurrentUser) -> Result<&User, AppError> {
    current.require(Permission::Upload)?;
    current.user.as_ref().ok_or(AppError::Unauthorized)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct NewQuery {
    /// A link to upload from, filled in (by the bookmarklet), which
    /// scripts send straight away.
    url: String,
    /// The page the link is from (the bookmarklet sends
    /// `document.referrer`).
    #[serde(rename = "ref")]
    referer: String,
}

async fn new(page: Page, Query(query): Query<NewQuery>) -> Result<Response, AppError> {
    uploader(&page.current)?;
    let allowance = upload::allowance(page.state(), &page.current).await?;
    let link = Link {
        url: &query.url,
        referer: &query.referer,
    };
    Ok(new_form(
        &page,
        link,
        None,
        StatusCode::OK,
        Some(&allowance),
    ))
}

/// The bookmarklet to drag to the toolbar, and the sites whose works'
/// pages are read.
async fn bookmarklet(page: Page) -> Response {
    let new_url = page
        .state()
        .config
        .server
        .public_url
        .join("/uploads/new")
        .map_or_else(|_| "/uploads/new".to_owned(), String::from);
    let script = format!(
        "javascript:location.href='{new_url}?url='+encodeURIComponent(location.href)\
         +'&ref='+encodeURIComponent(document.referrer)"
    );
    let mut sites: Vec<&moekura_core::sites::Site> = moekura_core::sites::ALL
        .iter()
        .copied()
        .filter(|site| crate::sources::READ.contains(&site.key))
        .collect();
    sites.sort_by_cached_key(|site| site.name.to_lowercase());
    sites.dedup_by_key(|site| site.name);
    page.render(
        "upload_bookmarklet.html",
        context! {
            script => script,
            sites => sites.iter().map(|site| context! {
                name => site.name,
                url => site.url,
            }).collect::<Vec<_>>(),
        },
    )
}

/// The link on the upload form, and the page it was found on.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Link<'a> {
    pub url: &'a str,
    pub referer: &'a str,
}

/// The upload form, with the link typed and what went wrong, if
/// anything.
pub(crate) fn new_form(
    page: &Page,
    link: Link<'_>,
    error: Option<&UploadError>,
    status: StatusCode,
    allowance: Option<&Allowance>,
) -> Response {
    let (message, duplicate_of) = match error {
        Some(UploadError::Duplicate(id)) => (None, Some(*id)),
        Some(e) => (Some(e.to_string()), None),
        None => (None, None),
    };
    page.render_with_status(
        status,
        "upload.html",
        context! {
            url => link.url,
            referer => link.referer,
            // A link given before the page was asked for is sent at once.
            send_now => error.is_none() && !link.url.is_empty(),
            error => message,
            duplicate_of => duplicate_of,
            max_mb => page.state().media.config().max_upload_mb,
            max_files => MAX_FILES,
            allowance => allowance.map(|a| context! {
                refusal => a.refusal,
                pending_left => a.pending_left,
                today_left => a.today_left,
            }),
        },
    )
}

/// The link sent to `/uploads` and the page it's from.
#[derive(Debug, Default)]
struct SentLink {
    url: String,
    referer: String,
}

impl SentLink {
    fn link(&self) -> Link<'_> {
        Link {
            url: &self.url,
            referer: &self.referer,
        }
    }
}

/// The files and the link sent to `/uploads`.
async fn receive(
    state: &AppState,
    mut multipart: Multipart,
) -> (SentLink, Result<Vec<TempUpload>, UploadError>) {
    let mut link = SentLink::default();
    let mut files = Vec::new();
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => return (link, Err(upload::multipart_error(state, &error))),
        };
        match field.name().unwrap_or_default() {
            // Browsers send an empty part when no file was chosen.
            "file" if field.file_name().is_some_and(|name| !name.is_empty()) => {
                if files.len() == MAX_FILES {
                    let error = format!("Upload at most {MAX_FILES} files at once.");
                    return (link, Err(UploadError::Invalid(error)));
                }
                match upload::save_to_temp(state, field).await {
                    Ok(file) => files.push(file),
                    Err(error) => return (link, Err(error)),
                }
            }
            name @ ("url" | "ref") => {
                let name = name.to_owned();
                match field.text().await {
                    Ok(text) if name == "url" => link.url = text.trim().to_owned(),
                    Ok(text) => link.referer = text.trim().to_owned(),
                    Err(error) => return (link, Err(upload::multipart_error(state, &error))),
                }
            }
            _ => {}
        }
    }
    (link, Ok(files))
}

async fn create(
    State(state): State<AppState>,
    page: Page,
    multipart: Multipart,
) -> Result<Response, AppError> {
    let user = uploader(&page.current)?.clone();
    let refuse = |link: Link<'_>, error: UploadError| {
        if let UploadError::Internal(detail) = &error {
            tracing::error!(error = %detail, "upload failed");
        }
        new_form(&page, link, Some(&error), error_status(&error), None)
    };
    if let Err(error) = upload::check_limits(&state, &page.current).await {
        return Ok(refuse(Link::default(), error));
    }
    let (mut sent, files) = match receive(&state, multipart).await {
        (sent, Ok(files)) => (sent, files),
        (sent, Err(error)) => return Ok(refuse(sent.link(), error)),
    };
    if sent.url.chars().count() > SOURCE_MAX_LEN {
        let error = format!("The link may be at most {SOURCE_MAX_LEN} characters.");
        return Ok(refuse(sent.link(), UploadError::Invalid(error)));
    }
    // Only a web page can have been where the link was found.
    if !is_web_link(&sent.referer) || sent.referer.chars().count() > SOURCE_MAX_LEN {
        sent.referer.clear();
    }
    let id = if !files.is_empty() {
        // A link sent with files says where they're from.
        stage_files(&state, user.id, &sent.url, &files).await?
    } else if is_web_link(&sent.url) {
        stage_link(&state, user.id, sent.link()).await?
    } else if sent.url.is_empty() {
        let error = UploadError::Invalid("Choose files to upload, or paste a link.".into());
        return Ok(refuse(sent.link(), error));
    } else {
        let error = UploadError::Invalid("That isn't a valid link.".into());
        return Ok(refuse(sent.link(), error));
    };
    tracing::info!(upload = id, user = user.name, "upload created");
    Ok(Redirect::to(&format!("/uploads/{id}")).into_response())
}

fn is_web_link(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}

/// Makes an upload of files sent from `source`, storing each. Returns the
/// upload's id.
async fn stage_files(
    state: &AppState,
    uploader_id: i64,
    source: &str,
    files: &[TempUpload],
) -> Result<i64, AppError> {
    let db = state.db.primary();
    let id = staged_uploads::create_upload(db, uploader_id, source, "").await?;
    for (position, file) in (0..).zip(files) {
        let slot = Slot {
            upload_id: id,
            uploader_id,
            position,
            file_name: file.name(),
            source,
        };
        let prepared = async {
            let prepared = upload::prepare(state, file).await?;
            Ok((prepared, upload::phash(state, file).await))
        }
        .await;
        match prepared {
            Ok((prepared, hash)) => {
                let mut tx = db.begin().await?;
                let staged = staged_uploads::create(&mut *tx, slot, prepared.stored(hash)).await?;
                crate::suggestions::queue_staged(state, &mut tx, staged).await?;
                tx.commit().await?;
            }
            Err(error) => {
                let (message, duplicate_of) = failure(&error);
                staged_uploads::create_failed(db, slot, &message, duplicate_of).await?;
            }
        }
    }
    Ok(id)
}

/// What a staged upload that failed with `error` says, and the post that
/// already has the file, if that's why.
fn failure(error: &UploadError) -> (String, Option<i64>) {
    match error {
        UploadError::Duplicate(post) => (error.to_string(), Some(*post)),
        UploadError::Internal(detail) => {
            tracing::error!(error = %detail, "staging an upload failed");
            ("Something went wrong on our side.".into(), None)
        }
        _ => (error.to_string(), None),
    }
}

/// Makes an upload of the files at `link`: a work's files when a source
/// strategy reads its page (or the page it was found on, for a bare
/// file), else the link itself. Each file's source is its
/// [canonical one](upload::file_source). They're downloaded in the
/// background; this waits a little for them. Returns the upload's id.
async fn stage_link(state: &AppState, uploader_id: i64, link: Link<'_>) -> Result<i64, AppError> {
    let url = link.url;
    let info = state.sources.lookup_from(url, link.referer).await;
    let (files, source) = match info.as_deref() {
        Some(info) if !info.files.is_empty() => (
            info.files.iter().take(MAX_LINK_FILES).cloned().collect(),
            info.page_url.clone(),
        ),
        _ => (vec![url.to_owned()], url.to_owned()),
    };
    let source: String = source.chars().take(SOURCE_MAX_LEN).collect();
    let mut tx = state.db.primary().begin().await?;
    let id = staged_uploads::create_upload(&mut *tx, uploader_id, &source, link.referer).await?;
    for (position, file_url) in (0..).zip(&files) {
        if file_url.chars().count() > SOURCE_MAX_LEN {
            continue;
        }
        let file_source = upload::file_source(file_url, info.as_deref(), url);
        let slot = Slot {
            upload_id: id,
            uploader_id,
            position,
            file_name: file_url,
            source: &file_source,
        };
        staged_uploads::create_pending(&mut *tx, slot, file_url).await?;
    }
    tx.commit().await?;
    let downloads = tokio::spawn(download_pending(state.clone(), id, info));
    // Downloads that take longer carry on; the page follows them.
    let _ = tokio::time::timeout(WAIT_FOR_DOWNLOADS, downloads).await;
    Ok(id)
}

/// Downloads and stores upload `upload_id`'s pending files, one at a
/// time. `info` is what the link's page said, if a strategy read it.
async fn download_pending(state: AppState, upload_id: i64, info: Option<Arc<SourceInfo>>) {
    let db = state.db.primary();
    let pending = match staged_uploads::of_upload(db, upload_id).await {
        Ok(files) => files,
        Err(error) => {
            tracing::error!(upload_id, %error, "upload's files not downloaded");
            return;
        }
    };
    for file in pending.iter().filter(|f| f.status == Status::Pending) {
        let url = file.file_url.as_deref().unwrap_or_default();
        let done = async {
            staged_uploads::started(db, file.id).await?;
            let fetched = async {
                let temp = upload::download(&state, url, info.as_deref()).await?;
                let prepared = upload::prepare(&state, &temp).await?;
                Ok::<_, UploadError>((prepared, upload::phash(&state, &temp).await))
            }
            .await;
            match fetched {
                Ok((prepared, hash)) => {
                    let mut tx = db.begin().await?;
                    staged_uploads::stored(&mut *tx, file.id, prepared.stored(hash)).await?;
                    crate::suggestions::queue_staged(&state, &mut tx, file.id).await?;
                    tx.commit().await
                }
                Err(error) => {
                    let (message, duplicate_of) = failure(&error);
                    staged_uploads::failed(db, file.id, &message, duplicate_of).await
                }
            }
        }
        .await;
        if let Err(error) = done {
            tracing::error!(upload_id, file = file.id, %error, "upload's file not downloaded");
        }
    }
}

/// Upload `id`, if it's `user`'s, and its files, giving up on downloads
/// that were abandoned.
async fn own_upload(
    state: &AppState,
    user: &User,
    id: i64,
) -> Result<(Upload, Vec<Staged>), AppError> {
    let db = state.db.primary();
    let upload = staged_uploads::upload_by_id(db, id)
        .await?
        .filter(|u| u.uploader_id == user.id)
        .ok_or(AppError::NotFound)?;
    let abandoned = "The download stopped before it finished; upload the link again.";
    staged_uploads::fail_abandoned(db, id, ABANDONED_AFTER, abandoned).await?;
    let files = staged_uploads::of_upload(db, id).await?;
    Ok((upload, files))
}

async fn show(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let user = uploader(&page.current)?;
    let state = page.state();
    let (upload, files) = own_upload(state, user, id).await?;
    // A single file is posted from the upload's own page.
    if let [file] = files.as_slice()
        && file.status != Status::Pending
    {
        let fields = AssetFields::for_file(state, &upload, file).await;
        return asset_page(&page, &upload, &files, 0, &fields, None).await;
    }
    let pending = files.iter().filter(|f| f.status == Status::Pending).count();
    let posted = files.iter().filter(|f| f.post_id.is_some()).count();
    Ok(page.render(
        "upload_files.html",
        context! {
            upload => upload_context(&upload, &files),
            files => file_cards(&page, &files).await?,
            pending => pending,
            posted => posted,
            refresh => (pending > 0).then_some(REFRESH_SECS),
        },
    ))
}

fn upload_context(upload: &Upload, files: &[Staged]) -> Value {
    context! {
        id => upload.id,
        url => url_value(&format!("/uploads/{}", upload.id)),
        source => upload.source,
        count => files.len(),
        date => crate::dates::day(upload.created_at),
    }
}

/// How a file can be previewed in a page: `image`, `video` or not at all.
fn preview_kind(media_type: &str) -> Option<&'static str> {
    match media_type {
        "jpeg" | "png" | "gif" | "webp" | "avif" => Some("image"),
        "mp4" | "webm" => Some("video"),
        _ => None,
    }
}

/// A file's details for pages.
fn file_context(state: &AppState, file: &Staged, thumb: Option<Value>) -> Value {
    let media_type = file.media_type.as_deref().unwrap_or_default();
    let original = file
        .storage_key
        .as_deref()
        .and_then(moekura_storage::Key::parse)
        .map(|key| state.file_url(&key));
    let name = file.file_name.rsplit('/').next().unwrap_or_default();
    context! {
        id => file.id,
        url => url_value(&format!("/uploads/{}/assets/{}", file.upload_id, file.id)),
        number => file.position + 1,
        name => if name.is_empty() { &file.file_name } else { name },
        full_name => file.file_name,
        status => file.status.as_str(),
        error => file.error,
        duplicate_of => file.duplicate_of,
        post_id => file.post_id,
        original => original.map(|u| url_value(&u)),
        preview => preview_kind(media_type),
        // A posted file's thumbnail, for lists.
        thumb => thumb,
        media_type => media_type,
        width => file.width,
        height => file.height,
        size => file.file_size.map(crate::posts::human_size),
        duration => file.duration_ms.map(|ms| format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)),
        animated => file.frames.is_some_and(|f| f > 1),
    }
}

/// `files` for a grid: posted ones with their post's thumbnail.
async fn file_cards(page: &Page, files: &[Staged]) -> Result<Vec<Value>, AppError> {
    let state = page.state();
    let posts: Vec<i64> = files.iter().filter_map(|f| f.post_id).collect();
    let cards = crate::posts::grid(page, state.db.primary(), &posts, None).await?;
    Ok(files
        .iter()
        .map(|file| {
            let card = file
                .post_id
                .and_then(|post| cards.iter().find(|(id, _)| *id == post))
                .map(|(_, card)| card.clone());
            file_context(state, file, card)
        })
        .collect())
}

/// The post form's fields.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
struct AssetFields {
    rating: String,
    tags: String,
    source: String,
    parent: String,
    description: String,
    commentary_title: String,
    commentary_description: String,
    /// A suggested tag clicked on (without scripts, which add it to the
    /// tags box): added to the form, which is shown again.
    add: String,
    /// The suggested rating, clicked on likewise.
    suggested_rating: String,
    /// "Check again" for suggestions: the form is shown again.
    refresh: String,
}

impl AssetFields {
    /// The form as it starts: the file's source, and the artist's
    /// commentary from its page when a source strategy reads it.
    async fn for_file(state: &AppState, upload: &Upload, file: &Staged) -> Self {
        let source = if file.source.is_empty() {
            upload.source.clone()
        } else {
            file.source.clone()
        };
        let mut fields = Self {
            source,
            ..Self::default()
        };
        if file.status == Status::Ready
            && file.post_id.is_none()
            && is_web_link(&fields.source)
            && let Some(info) = state.sources.lookup(&fields.source).await
        {
            fields.commentary_title.clone_from(&info.title);
            fields.commentary_description.clone_from(&info.description);
        }
        fields
    }

    fn upload_fields(&self) -> UploadFields {
        UploadFields {
            rating: self.rating.parse().ok(),
            tags: self.tags.clone(),
            source: self.source.trim().to_owned(),
            parent: self.parent.trim().to_owned(),
            description: self.description.trim().to_owned(),
            commentary_title: self.commentary_title.trim().to_owned(),
            commentary_description: self.commentary_description.trim().to_owned(),
            ..UploadFields::default()
        }
    }
}

/// Upload `id`'s file `file_id`, if both are `user`'s, with the upload's
/// files and its place among them.
async fn own_file(
    state: &AppState,
    user: &User,
    id: i64,
    file_id: i64,
) -> Result<(Upload, Vec<Staged>, usize), AppError> {
    let (upload, files) = own_upload(state, user, id).await?;
    let index = files
        .iter()
        .position(|f| f.id == file_id)
        .ok_or(AppError::NotFound)?;
    Ok((upload, files, index))
}

async fn asset(page: Page, Path((id, file_id)): Path<(i64, i64)>) -> Result<Response, AppError> {
    let user = uploader(&page.current)?;
    let state = page.state();
    let (upload, files, index) = own_file(state, user, id, file_id).await?;
    let fields = AssetFields::for_file(state, &upload, &files[index]).await;
    asset_page(&page, &upload, &files, index, &fields, None).await
}

async fn post_asset(
    page: Page,
    Path((id, file_id)): Path<(i64, i64)>,
    Form(fields): Form<AssetFields>,
) -> Result<Response, AppError> {
    let user = uploader(&page.current)?;
    let state = page.state();
    let (upload, files, index) = own_file(state, user, id, file_id).await?;
    // A suggestion taken, or a check for them, without scripts: the form
    // again, with the suggestion in it, rather than a post.
    if !(fields.add.is_empty() && fields.suggested_rating.is_empty() && fields.refresh.is_empty()) {
        let mut fields = fields;
        let added = std::mem::take(&mut fields.add);
        if !added.trim().is_empty() {
            fields.tags = format!("{} {} ", fields.tags.trim_end(), added.trim())
                .trim_start()
                .to_owned();
        }
        if !fields.suggested_rating.is_empty() {
            fields.rating = std::mem::take(&mut fields.suggested_rating);
        }
        return asset_page(&page, &upload, &files, index, &fields, None).await;
    }
    let upload_fields = fields.upload_fields();
    let posted = if files[index].status != Status::Ready {
        Err(UploadError::Invalid("This file can't be posted.".into()))
    } else {
        match upload::check_limits(state, &page.current).await {
            Ok(()) => upload::post_staged(state, &page.current, file_id, &upload_fields).await,
            Err(error) => Err(error),
        }
    };
    match posted {
        Ok(post_id) => {
            let kept = crate::tag_warnings::kept_categories(
                state.db.primary(),
                &upload_fields.tags,
                post_id,
            )
            .await?;
            let query = crate::tag_warnings::check_query(&kept);
            Ok(Redirect::to(&format!("/posts/{post_id}?{query}")).into_response())
        }
        Err(error) => {
            if let UploadError::Internal(detail) = &error {
                tracing::error!(error = %detail, "posting an upload failed");
            }
            asset_page(&page, &upload, &files, index, &fields, Some(&error)).await
        }
    }
}

/// The tagger's suggestions for a file, as the box on its post form
/// holds them, for scripts waiting for them. Empty when there are none.
async fn asset_suggestions(
    page: Page,
    Path((id, file_id)): Path<(i64, i64)>,
) -> Result<Response, AppError> {
    let user = uploader(&page.current)?;
    let state = page.state();
    let (_, files, index) = own_file(state, user, id, file_id).await?;
    let file = &files[index];
    let suggestions = if file.status == Status::Ready && file.post_id.is_none() {
        crate::suggestions::for_upload_form(state, state.db.primary(), file.id, "", "").await?
    } else {
        None
    };
    Ok(page.render(
        "upload_suggestions.html",
        context! { suggestions => suggestions },
    ))
}

/// The wiki page about a problem with a link on `site` (a site's key):
/// `<specific>` when that page exists, else `general`.
async fn help_page(
    state: &AppState,
    specific: Option<String>,
    general: &str,
) -> Result<String, AppError> {
    if let Some(title) = specific
        && moekura_db::wiki::by_title(state.db.primary(), &title)
            .await?
            .is_some()
    {
        return Ok(title);
    }
    Ok(general.to_owned())
}

/// Most pixel-perfect duplicates named.
const DUPLICATES_SHOWN: i64 = 20;

/// What the post form warns about a file, as Danbooru's badges do: it was
/// sent from disk (no source), its source is an image rather than its
/// page, it's a resized copy, it was made by an image generator, or posts
/// have exactly its pixels. `source` is the form's.
async fn warnings(page: &Page, file: &Staged, source: &str) -> Result<Value, AppError> {
    use moekura_core::file_traits::FileTrait;
    use moekura_core::sites;

    let state = page.state();
    let db = state.db.primary();
    let no_source = file.file_url.is_none() && file.source.is_empty();
    let bad_link = match sites::parse(source).filter(|u| u.is_file && u.page_url.is_none()) {
        Some(found) => {
            let specific = format!("bad_{}_link", found.site.key);
            Some(help_page(state, Some(specific), "bad_link").await?)
        }
        None => None,
    };
    let downloaded = file.file_url.as_deref().unwrap_or(&file.file_name);
    let sample = match sites::parse(downloaded).filter(|u| u.is_sample) {
        Some(found) => {
            let specific = format!("{}_sample", found.site.key);
            Some(help_page(state, Some(specific), "image_sample").await?)
        }
        None => None,
    };
    let traits = FileTrait::from_stored(&file.traits);
    let duplicates = match &file.pixel_hash {
        Some(hash) => {
            let ids = moekura_db::media::posts_with_pixel_hash(db, hash, DUPLICATES_SHOWN).await?;
            let visible = crate::posts::visibility(&page.current);
            moekura_db::posts::by_ids(db, &ids)
                .await?
                .into_iter()
                .filter(|p| visible.allows(p))
                .map(|p| p.id)
                .collect::<Vec<i64>>()
        }
        None => Vec::new(),
    };
    let duplicates_search = (duplicates.len() > 1).then(|| {
        let ids: Vec<String> = duplicates.iter().map(i64::to_string).collect();
        url_value(&format!("/posts?tags=id%3A{}", ids.join(",")))
    });
    Ok(context! {
        no_source => no_source,
        bad_link => bad_link,
        sample => sample,
        ai_generated => traits.contains(&FileTrait::AiGenerated),
        duplicates => duplicates,
        duplicates_search => duplicates_search,
        any => no_source || bad_link.is_some() || sample.is_some()
            || traits.contains(&FileTrait::AiGenerated) || !duplicates.is_empty(),
    })
}

/// Links searching other sites for `original`, the file's absolute
/// address, and this one's image search.
fn search_links(original: &str) -> Value {
    let encoded: String = url::form_urlencoded::byte_serialize(original.as_bytes()).collect();
    let elsewhere: Vec<Value> = [
        (
            "SauceNAO",
            format!("https://saucenao.com/search.php?url={encoded}"),
        ),
        (
            "Ascii2D",
            format!("https://ascii2d.net/search/url/{encoded}"),
        ),
        (
            "Yandex",
            format!("https://yandex.com/images/search?rpt=imageview&url={encoded}"),
        ),
        (
            "Google Lens",
            format!("https://lens.google.com/uploadbyurl?url={encoded}"),
        ),
        (
            "Bing",
            format!("https://www.bing.com/images/searchbyimage?cbir=sbi&imgurl={encoded}"),
        ),
    ]
    .into_iter()
    .map(|(name, url)| context! { name => name, url => url_value(&url) })
    .collect();
    context! {
        saucenao => elsewhere[0].get_attr("url").ok(),
        elsewhere => elsewhere,
        here => url_value(&format!("/iqdb_queries?url={encoded}")),
    }
}

/// The page of file `files[index]` of `upload`: the file, the posts it
/// looks like, and the form making it a post.
async fn asset_page(
    page: &Page,
    upload: &Upload,
    files: &[Staged],
    index: usize,
    fields: &AssetFields,
    error: Option<&UploadError>,
) -> Result<Response, AppError> {
    let state = page.state();
    let file = &files[index];
    let similar = if file.status == Status::Ready && file.post_id.is_none() {
        let hash = file.phash.map(|h| h as u64);
        let posts = upload::lookalikes(state, &page.current, hash, None)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        crate::posts::grid(page, state.db.primary(), &posts, None)
            .await?
            .into_iter()
            .map(|(_, card)| card)
            .collect()
    } else {
        Vec::new()
    };
    let suggestions = if file.status == Status::Ready && file.post_id.is_none() {
        crate::suggestions::for_upload_form(
            state,
            state.db.primary(),
            file.id,
            &fields.tags,
            &fields.rating,
        )
        .await?
    } else {
        None
    };
    let link = |i: usize| {
        files
            .get(i)
            .map(|f| url_value(&format!("/uploads/{}/assets/{}", upload.id, f.id)))
    };
    let (message, duplicate_of) = match error {
        Some(UploadError::Duplicate(id)) => (None, Some(*id)),
        Some(UploadError::Internal(_)) => (
            Some("Something went wrong on our side. Please try again.".to_owned()),
            None,
        ),
        Some(e) => (Some(e.to_string()), None),
        None => (None, None),
    };
    let ratings: Vec<_> = Rating::ALL
        .iter()
        .map(|r| context! { code => r.code() })
        .collect();
    let status = error.map_or(StatusCode::OK, error_status);
    let open = file.status == Status::Ready && file.post_id.is_none();
    let warnings = if open {
        Some(warnings(page, file, &fields.source).await?)
    } else {
        None
    };
    // Other sites fetch the file, so they're given its full address.
    let absolute = file
        .storage_key
        .as_deref()
        .and_then(moekura_storage::Key::parse)
        .map(|key| state.file_url(&key))
        .and_then(|url| state.config.server.public_url.join(&url).ok())
        .map(String::from);
    Ok(page.render_with_status(
        status,
        "upload_asset.html",
        context! {
            upload => upload_context(upload, files),
            file => file_context(state, file, None),
            previous_url => index.checked_sub(1).and_then(link),
            next_url => link(index + 1),
            similar => similar,
            warnings => warnings,
            search => absolute.as_deref().map(search_links),
            suggestions => suggestions,
            suggestions_url => url_value(&format!(
                "/uploads/{}/assets/{}/suggestions",
                upload.id, file.id
            )),
            ratings => ratings,
            form => context! {
                rating => fields.rating,
                tags => fields.tags,
                source => fields.source,
                parent => fields.parent,
                description => fields.description,
                commentary_title => fields.commentary_title,
                commentary_description => fields.commentary_description,
            },
            error => message,
            duplicate_of => duplicate_of,
            refresh => (file.status == Status::Pending).then_some(REFRESH_SECS),
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    page: Option<i64>,
}

/// The user's uploaded files, newest first.
async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    let user = uploader(&page.current)?;
    let number = query.page.unwrap_or(1).clamp(1, 10_000);
    let mut files = staged_uploads::by_uploader(
        page.state().db.primary(),
        user.id,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let more = files.len() > PAGE_SIZE as usize;
    files.truncate(PAGE_SIZE as usize);
    let list_url = |n: i64| url_value(&format!("/uploads?page={n}"));
    Ok(page.render(
        "uploads.html",
        context! {
            files => file_cards(&page, &files).await?,
            previous_url => (number > 1).then(|| list_url(number - 1)),
            next_url => more.then(|| list_url(number + 1)),
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, fixture, session_for, test_state};

    async fn app(pool: &PgPool) -> (TestApp, AppState) {
        let state = test_state(pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let routes = routes(max)
            .merge(upload::routes(max))
            .merge(crate::posts::routes());
        (TestApp::new(state.clone(), routes), state)
    }

    /// The id in a `/uploads/<id>` redirect.
    fn upload_in(location: Option<&str>) -> i64 {
        location
            .and_then(|l| l.strip_prefix("/uploads/"))
            .and_then(|id| id.parse().ok())
            .expect("a redirect to an upload")
    }

    /// The id in a `/posts/<id>?…` redirect.
    fn post_in(location: Option<&str>) -> i64 {
        location
            .and_then(|l| l.strip_prefix("/posts/"))
            .and_then(|rest| rest.split('?').next())
            .and_then(|id| id.parse().ok())
            .expect("a redirect to a post")
    }

    async fn files_of(pool: &PgPool, upload: i64) -> Vec<Staged> {
        staged_uploads::of_upload(pool, upload).await.unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn files_are_staged_then_each_posted(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let (a, b) = (fixture::png(40, 30), fixture::png(50, 30));
        let sent = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[("url", "https://example.com/work/1".to_owned())],
                &[("file", "first.png", &a), ("file", "second.png", &b)],
            )
            .await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let upload = upload_in(sent.location.as_deref());
        let files = files_of(&pool, upload).await;
        assert_eq!(
            files
                .iter()
                .map(|f| (f.file_name.as_str(), f.status, f.width))
                .collect::<Vec<_>>(),
            [
                ("first.png", Status::Ready, Some(40)),
                ("second.png", Status::Ready, Some(50))
            ]
        );
        assert!(files.iter().all(|f| f.phash.is_some()));

        let page = app.get(&format!("/uploads/{upload}"), Some(&alice)).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("2 files, 0 posted"), "{}", page.body);
        let first = format!("/uploads/{upload}/assets/{}", files[0].id);
        assert!(
            page.body.contains(&format!("href=\"{first}\"")),
            "{}",
            page.body
        );

        // Each file has its page, with a preview, its details, the link
        // as its source, and a way to the next.
        let form = app.get(&first, Some(&alice)).await;
        assert_eq!(form.status, StatusCode::OK, "{}", form.body);
        assert!(form.body.contains("File 1 of 2"), "{}", form.body);
        assert!(
            form.body.contains("<img src=\"/data/original/"),
            "{}",
            form.body
        );
        assert!(form.body.contains("40×30"), "{}", form.body);
        assert!(
            form.body
                .contains("value=\"https:&#x2f;&#x2f;example.com&#x2f;work&#x2f;1\""),
            "{}",
            form.body
        );
        assert!(
            form.body
                .contains(&format!("/uploads/{upload}/assets/{}", files[1].id)),
            "{}",
            form.body
        );

        // Mistakes are explained, and what was typed stays.
        let refused = app
            .post_form(
                &first,
                Some(&alice),
                &[],
                "rating=s&tags=cat+solo&parent=abc&source=https%3A%2F%2Fexample.com%2Fwork%2F1",
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            refused.body.contains("The parent must be a post number."),
            "{}",
            refused.body
        );
        assert!(
            refused.body.contains(">cat solo</textarea>"),
            "{}",
            refused.body
        );

        let posted = app
            .post_form(
                &first,
                Some(&alice),
                &[],
                "rating=s&tags=cat+solo&description=hello&source=https%3A%2F%2Fexample.com%2Fwork%2F1",
            )
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let post_id = post_in(posted.location.as_deref());
        let post = moekura_db::posts::by_id(&pool, post_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (post.rating, post.source.as_str(), post.description.as_str()),
            (Rating::Sensitive, "https://example.com/work/1", "hello")
        );
        let asset = moekura_db::media::for_post(&pool, post_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(asset.storage_key, files[0].storage_key.clone().unwrap());

        // The second can name the first as its parent.
        let second = format!("/uploads/{upload}/assets/{}", files[1].id);
        let child = app
            .post_form(
                &second,
                Some(&alice),
                &[],
                &format!("rating=s&tags=cat&parent=%23{post_id}"),
            )
            .await;
        assert_eq!(child.status, StatusCode::SEE_OTHER, "{}", child.body);
        let child = moekura_db::posts::by_id(&pool, post_in(child.location.as_deref()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.parent_id, Some(post_id));

        let page = app.get(&format!("/uploads/{upload}"), Some(&alice)).await;
        assert!(page.body.contains("2 files, 2 posted"), "{}", page.body);
        // Posted once only.
        let form = app.get(&first, Some(&alice)).await;
        assert!(
            form.body.contains("This file was posted as"),
            "{}",
            form.body
        );
        assert!(!form.body.contains("name=\"rating\""), "{}", form.body);
        let again = app
            .post_form(&first, Some(&alice), &[], "rating=s&tags=cat")
            .await;
        assert!(
            again.body.contains(&format!("post #{post_id}")),
            "{}",
            again.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn uploads_are_their_uploaders_own(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let sent = app
            .post_multipart(
                "/uploads",
                Some(&alice),
                &[],
                Some(("a.png", &fixture::png(40, 30))),
            )
            .await;
        let upload = upload_in(sent.location.as_deref());
        let file = files_of(&pool, upload).await[0].id;
        // A single file's form is the upload's page.
        let page = app.get(&format!("/uploads/{upload}"), Some(&alice)).await;
        assert!(page.body.contains("name=\"rating\""), "{}", page.body);
        assert!(!page.body.contains("File 1 of 1"), "{}", page.body);
        assert!(
            page.body
                .contains(&format!("action=\"/uploads/{upload}/assets/{file}\"")),
            "{}",
            page.body
        );

        for path in [
            format!("/uploads/{upload}"),
            format!("/uploads/{upload}/assets/{file}"),
        ] {
            assert_eq!(
                app.get(&path, Some(&bob)).await.status,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        let theirs = app
            .post_form(
                &format!("/uploads/{upload}/assets/{file}"),
                Some(&bob),
                &[],
                "rating=s",
            )
            .await;
        assert_eq!(theirs.status, StatusCode::NOT_FOUND);
        // Another upload's number doesn't reach the file either.
        let other = app
            .post_multipart(
                "/uploads",
                Some(&alice),
                &[],
                Some(("b.png", &fixture::png(44, 30))),
            )
            .await;
        let other = upload_in(other.location.as_deref());
        assert_eq!(
            app.get(&format!("/uploads/{other}/assets/{file}"), Some(&alice))
                .await
                .status,
            StatusCode::NOT_FOUND
        );

        // Listed among their uploads, newest first.
        let mine = app.get("/uploads", Some(&alice)).await;
        assert_eq!(mine.status, StatusCode::OK);
        let (newer, older) = (
            mine.body.find(&format!("/uploads/{other}/assets/")),
            mine.body.find(&format!("/uploads/{upload}/assets/{file}")),
        );
        assert!(newer.unwrap() < older.unwrap(), "{}", mine.body);
        let empty = app.get("/uploads", Some(&bob)).await;
        assert!(
            empty.body.contains("You haven't uploaded anything yet."),
            "{}",
            empty.body
        );
        assert_eq!(
            app.get("/uploads", None).await.location.as_deref(),
            Some("/login?next=%2Fuploads")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn files_that_cant_be_posted_say_why(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(40, 30);
        let existing = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &[("rating", "g".to_owned())],
                Some(("a.png", &png)),
            )
            .await;
        let existing = post_in(existing.location.as_deref());
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec();
        let good = fixture::png(60, 30);
        let sent = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[],
                &[
                    ("file", "again.png", &png),
                    ("file", "x.svg", &svg),
                    ("file", "new.png", &good),
                ],
            )
            .await;
        let upload = upload_in(sent.location.as_deref());
        let files = files_of(&pool, upload).await;
        assert_eq!(
            files.iter().map(|f| f.status).collect::<Vec<_>>(),
            [Status::Failed, Status::Failed, Status::Ready]
        );
        assert_eq!(files[0].duplicate_of, Some(existing));
        let page = app.get(&format!("/uploads/{upload}"), Some(&alice)).await;
        assert!(
            page.body.contains(&format!(
                "already uploaded as <a href=\"/posts/{existing}\""
            )),
            "{}",
            page.body
        );
        assert!(
            page.body.contains("This file type isn&#x27;t supported"),
            "{}",
            page.body
        );
        let failed = format!("/uploads/{upload}/assets/{}", files[1].id);
        let form = app.get(&failed, Some(&alice)).await;
        assert!(!form.body.contains("name=\"rating\""), "{}", form.body);
        let refused = app.post_form(&failed, Some(&alice), &[], "rating=s").await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);

        // Nothing sent, too many files, or a bad link: nothing is made.
        let nothing = app
            .post_multipart("/uploads", Some(&alice), &[], None)
            .await;
        assert!(
            nothing
                .body
                .contains("Choose files to upload, or paste a link."),
            "{}",
            nothing.body
        );
        let many: Vec<(&str, &str, &[u8])> = (0..=MAX_FILES)
            .map(|_| ("file", "a.png", good.as_slice()))
            .collect();
        let too_many = app
            .post_multipart_files("/uploads", Some(&alice), &[], &many)
            .await;
        assert_eq!(too_many.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            too_many.body.contains("at most 20 files"),
            "{}",
            too_many.body
        );
        let bad = app
            .post_multipart(
                "/uploads",
                Some(&alice),
                &[("url", "file:///etc/passwd".to_owned())],
                None,
            )
            .await;
        assert!(
            bad.body.contains("That isn&#x27;t a valid link."),
            "{}",
            bad.body
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM uploads")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn bookmarklet_links_bring_the_page_they_were_on(pool: PgPool) {
        let png = fixture::png(40, 30);
        let origin = Router::new().route("/img/1.png", get(move || async move { png }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let mut state = test_state(&pool).await;
        state.fetcher = crate::fetch::Fetcher::new(Duration::from_secs(10), true);
        state.sources = Arc::new(crate::sources::Sources::new(
            true,
            moekura_core::config::SourcesConfig::default(),
        ));
        let (work, file) = (
            format!("http://{addr}/work"),
            format!("http://{addr}/img/1.png"),
        );
        state.sources.remember(
            &work,
            SourceInfo {
                site: "Example",
                page_url: work.clone(),
                files: vec![file.clone()],
                ..SourceInfo::default()
            },
        );
        let max = 10 * 1024 * 1024;
        let app = TestApp::new(state.clone(), routes(max).merge(upload::routes(max)));
        let alice = session_for(&pool, "alice", SystemRole::Member).await;

        // The bookmarklet opens the form with the link, which scripts send.
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("url", &file)
            .append_pair("ref", &work)
            .finish();
        let form = app
            .get(&format!("/uploads/new?{query}"), Some(&alice))
            .await;
        assert!(form.body.contains("data-upload-send-now"), "{}", form.body);
        assert!(form.body.contains("name=\"ref\""), "{}", form.body);
        let plain = app.get("/uploads/new", Some(&alice)).await;
        assert!(!plain.body.contains("data-upload-send-now"));

        // The bare image's work is the page it was found on.
        let sent = app
            .post_multipart(
                "/uploads",
                Some(&alice),
                &[("url", file.clone()), ("ref", work.clone())],
                None,
            )
            .await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let upload = upload_in(sent.location.as_deref());
        let files = files_of(&pool, upload).await;
        assert_eq!(files[0].source, work);
        assert_eq!(files[0].status, Status::Ready);
        let saved = staged_uploads::upload_by_id(&pool, upload)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.referer_url, work);

        // Without it, the image is its own source; a page that isn't a
        // web page isn't kept.
        let sent = app
            .post_multipart(
                "/uploads",
                Some(&alice),
                &[("url", file.clone()), ("ref", "javascript:x".to_owned())],
                None,
            )
            .await;
        let upload = upload_in(sent.location.as_deref());
        assert_eq!(files_of(&pool, upload).await[0].source, file);
        let saved = staged_uploads::upload_by_id(&pool, upload)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.referer_url, "");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_bookmarklet_page_lists_the_sites_read(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let page = app.get("/uploads/bookmarklet", None).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(
            page.body.contains("href=\"javascript:location.href="),
            "{}",
            page.body
        );
        assert!(
            page.body.contains("&#x2f;uploads&#x2f;new?url="),
            "{}",
            page.body
        );
        for site in ["Pixiv", "X", "Fantia"] {
            assert!(page.body.contains(&format!(">{site}</a>")), "{site}");
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_post_form_warns_about_the_file(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let posted = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &[("rating", "g".to_owned())],
                Some(("a.png", &fixture::png(64, 48))),
            )
            .await;
        let original = post_in(posted.location.as_deref());

        // The same pixels with a generator's parameters, from disk.
        let generated = fixture::png_with_chunk(64, 48, "parameters", "1girl, masterpiece");
        let sent = app
            .post_multipart("/uploads", Some(&alice), &[], Some(("b.png", &generated)))
            .await;
        let upload = upload_in(sent.location.as_deref());
        let file = &files_of(&pool, upload).await[0];
        assert_eq!(file.traits, ["ai_generated"]);
        assert!(file.pixel_hash.is_some());
        let form = app
            .get(&format!("/uploads/{upload}"), Some(&alice))
            .await
            .body;
        for badge in ["No Source", "AI-Generated", "Pixel-Perfect Duplicate"] {
            assert!(form.contains(&format!("{badge}</a>")), "{badge}: {form}");
        }
        assert!(
            form.contains(&format!(
                "exactly the pixels of <a href=\"/posts/{original}\""
            )),
            "{form}"
        );
        assert!(
            form.contains("href=\"https://saucenao.com/search.php?url=http%3A%2F%2F"),
            "{form}"
        );
        assert!(form.contains("data-copy-text=\""), "{form}");
        assert!(!form.contains("Bad Source"), "{form}");

        // A link to an image sample that doesn't name its page.
        let link = "https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb.jpg";
        let sample = staged_uploads::create_upload(&pool, file.uploader_id, link, "")
            .await
            .unwrap();
        let slot = Slot {
            upload_id: sample,
            uploader_id: file.uploader_id,
            position: 0,
            file_name: link,
            source: link,
        };
        let id = staged_uploads::create_pending(&pool, slot, link)
            .await
            .unwrap();
        let mut stored = upload::Prepared::from_staged(file).unwrap();
        stored.pixel_hash = None;
        stored.traits.clear();
        staged_uploads::stored(&pool, id, stored.stored(None))
            .await
            .unwrap();
        let form = app
            .get(&format!("/uploads/{sample}"), Some(&alice))
            .await
            .body;
        assert!(form.contains("href=\"/wiki/bad_link\""), "{form}");
        assert!(form.contains("Image Sample</a>"), "{form}");
        assert!(!form.contains("No Source"), "{form}");
        assert!(!form.contains("AI-Generated"), "{form}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn a_works_files_are_downloaded(pool: PgPool) {
        let (a, b) = (fixture::png(40, 30), fixture::png(48, 30));
        let origin = Router::new()
            .route("/1.png", get(move || async move { a }))
            .route("/2.png", get(move || async move { b }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let mut state = test_state(&pool).await;
        state.fetcher = crate::fetch::Fetcher::new(Duration::from_secs(10), true);
        state.sources = Arc::new(crate::sources::Sources::new(
            true,
            moekura_core::config::SourcesConfig::default(),
        ));
        let page = format!("http://{addr}/work");
        state.sources.remember(
            &page,
            SourceInfo {
                site: "Example",
                page_url: page.clone(),
                files: vec![
                    format!("http://{addr}/1.png"),
                    format!("http://{addr}/2.png"),
                    format!("http://{addr}/missing.png"),
                ],
                ..SourceInfo::default()
            },
        );
        let max = 10 * 1024 * 1024;
        let app = TestApp::new(state.clone(), routes(max).merge(upload::routes(max)));
        let alice = session_for(&pool, "alice", SystemRole::Member).await;

        let sent = app
            .post_multipart("/uploads", Some(&alice), &[("url", page.clone())], None)
            .await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let upload = upload_in(sent.location.as_deref());
        // Local downloads finish while the request waits.
        let files = files_of(&pool, upload).await;
        assert_eq!(
            files
                .iter()
                .map(|f| (f.status, f.width, f.source.as_str()))
                .collect::<Vec<_>>(),
            [
                (Status::Ready, Some(40), page.as_str()),
                (Status::Ready, Some(48), page.as_str()),
                (Status::Failed, None, page.as_str())
            ]
        );
        assert!(
            files[2].error.as_deref().is_some_and(|e| e.contains("404")),
            "{:?}",
            files[2].error
        );
        let shown = app.get(&format!("/uploads/{upload}"), Some(&alice)).await;
        assert!(shown.body.contains("3 files, 0 posted"), "{}", shown.body);
        assert!(
            !shown.body.contains("http-equiv=\"refresh\""),
            "{}",
            shown.body
        );

        // A download nobody finished is given up on; until then the page
        // keeps reloading.
        let stuck = staged_uploads::create_pending(
            &pool,
            Slot {
                upload_id: upload,
                uploader_id: files[0].uploader_id,
                position: 3,
                file_name: "x",
                source: "",
            },
            "http://example.com/x.png",
        )
        .await
        .unwrap();
        let waiting = app.get(&format!("/uploads/{upload}"), Some(&alice)).await;
        assert!(
            waiting.body.contains("1 is still downloading"),
            "{}",
            waiting.body
        );
        assert!(
            waiting.body.contains("http-equiv=\"refresh\""),
            "{}",
            waiting.body
        );
        sqlx::query(
            "UPDATE staged_uploads SET updated_at = now() - interval '1 hour' WHERE id = $1",
        )
        .bind(stuck)
        .execute(&pool)
        .await
        .unwrap();
        let given_up = app.get(&format!("/uploads/{upload}"), Some(&alice)).await;
        assert!(
            given_up.body.contains("The download stopped"),
            "{}",
            given_up.body
        );
    }
}
