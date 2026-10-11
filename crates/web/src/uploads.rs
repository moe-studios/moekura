//! Uploading in two steps, as on Danbooru. `/uploads/new` takes files
//! (several at once) or a link, which become an upload: its files are
//! stored and wait until each is posted from its own form at
//! `/uploads/{id}/assets/{file}`, shown beside a preview, its details and
//! the posts it looks like. A link to a work of several files (a Pixiv
//! gallery) downloads them in the background, and the upload's page
//! shows how far that got. `/uploads` lists the user's files.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Multipart, Path, Query, State};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use hmac::{Hmac, KeyInit, Mac};
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::posts::{Rating, SOURCE_MAX_LEN};
use moekura_db::posts;
use moekura_db::staged_uploads::{self, Slot, Staged, Status, Upload};
use moekura_db::users::User;
use serde::Deserialize;
use sha2::Sha256;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, SemaphorePermit};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::sources::SourceInfo;
use crate::templates::url_value;
use crate::upload::{self, Allowance, TempUpload, UploadError, UploadFields, error_status};

/// Most files sent at once, and taken from a link to a work unless the
/// uploader asks for all of them.
pub const MAX_FILES: usize = 20;
/// Most files taken from a link to a work.
const MAX_LINK_FILES: usize = 100;
/// Files downloaded from links at once, all uploads together; the rest
/// wait their turn.
const MAX_DOWNLOADS: usize = 8;
/// The turns to download, [`MAX_DOWNLOADS`] of them.
static DOWNLOADS: Semaphore = Semaphore::const_new(MAX_DOWNLOADS);
/// Of those, the most one user's uploads take at once, so others' get
/// turns too.
const MAX_USER_DOWNLOADS: usize = 2;
/// Each user's turns, [`MAX_USER_DOWNLOADS`] of them, while their
/// downloads go on.
static USER_DOWNLOADS: LazyLock<Mutex<HashMap<i64, Arc<Semaphore>>>> =
    LazyLock::new(Mutex::default);
/// How often files waiting for a turn to download are marked as still
/// wanted, so they aren't taken for abandoned (see [`ABANDONED_AFTER`]).
const STILL_WAITING: Duration = Duration::from_secs(60);
/// Most files a user may have waiting to be posted.
pub const MAX_WAITING: i64 = 250;
/// How long creating an upload from a link waits for its first file
/// before showing the upload's page, which follows the rest.
const WAIT_FOR_DOWNLOADS: Duration = Duration::from_secs(8);
/// A file still waiting this long after its download was last worked on
/// was abandoned (the server restarted meanwhile).
const ABANDONED_AFTER: Duration = Duration::from_secs(10 * 60);
/// Scratch files this old were left behind (see [`sweep_work_dir`]).
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// Files on a page of `/uploads`.
const PAGE_SIZE: i64 = 48;
/// How often the page of an upload still downloading reloads.
const REFRESH_SECS: u32 = 3;

pub fn routes(max_upload_bytes: u64) -> Router<AppState> {
    let body_limit = upload::body_limit(max_upload_bytes.saturating_mul(MAX_FILES as u64));
    Router::new()
        .route("/uploads", get(index).post(create).layer(body_limit))
        .route("/uploads/new", get(new))
        .route(
            "/uploads/bookmarklet",
            get(bookmarklet).post(new_bookmarklet_key),
        )
        .route("/uploads/{id}", get(show))
        .route("/uploads/{id}/status", get(status))
        .route("/uploads/{id}/assets/{file}", get(asset).post(post_asset))
        .route(
            "/uploads/{id}/assets/{file}/suggestions",
            get(asset_suggestions),
        )
        .route("/uploads/source-data", get(source_data_fragment))
}

/// How many more files `user` may have waiting to be posted; an error
/// when none.
pub(crate) async fn room(state: &AppState, user: &User) -> Result<usize, UploadError> {
    let waiting = staged_uploads::waiting(state.db.primary(), user.id).await?;
    if waiting >= MAX_WAITING {
        return Err(UploadError::Limit(format!(
            "You have {waiting} files waiting to be posted (or that failed in the last hour), \
             the most you may have. Post some first, or wait for those you don't post to expire."
        )));
    }
    Ok(usize::try_from(MAX_WAITING - waiting).unwrap_or(0))
}

/// The uploading user: uploads are kept for an account.
fn uploader(current: &CurrentUser) -> Result<&User, AppError> {
    current.require(Permission::Upload)?;
    current.user.as_ref().ok_or(AppError::Unauthorized)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct NewQuery {
    /// A link to upload from, filled in (by the bookmarklet).
    url: String,
    /// The page the link is from (the bookmarklet sends
    /// `document.referrer`).
    #[serde(rename = "ref")]
    referer: String,
    /// The user's [bookmarklet token](bookmarklet_token), with which
    /// scripts send the link straight away. Without it the link waits for
    /// the button: any site can link here with one.
    token: String,
}

async fn new(page: Page, Query(query): Query<NewQuery>) -> Result<Response, AppError> {
    let user = uploader(&page.current)?;
    let state = page.state();
    let allowance = upload::allowance(state, &page.current).await?;
    let (new_uploader, _) = guidance(state, user).await?;
    let link = Link {
        url: &query.url,
        referer: &query.referer,
    };
    let send_now =
        !query.url.is_empty() && is_bookmarklet_token(state, user.id, &query.token).await?;
    let notes = FormNotes {
        send_now,
        allowance: Some(&allowance),
        new_uploader,
        many_files: None,
    };
    Ok(form_page(&page, link, None, StatusCode::OK, &notes))
}

/// What user `user_id`'s bookmarklet token is made with is called in
/// `moekura_db::secrets`. Each user has their own, so that one can make a
/// new one (see [`new_bookmarklet_key`]) without stopping others'.
fn bookmarklet_key(user_id: i64) -> String {
    format!("bookmarklet:{user_id}")
}

/// The MAC behind user `user_id`'s bookmarklet token.
async fn bookmarklet_mac(state: &AppState, user_id: i64) -> Result<Hmac<Sha256>, AppError> {
    let key = moekura_db::secrets::get_or_create(
        state.db.primary(),
        &bookmarklet_key(user_id),
        moekura_core::tokens::NewToken::generate().hash,
    )
    .await?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC takes keys of any length");
    mac.update(b"bookmarklet\n");
    mac.update(user_id.to_string().as_bytes());
    Ok(mac)
}

/// User `user_id`'s bookmarklet token. Their bookmarklet carries it, so
/// the upload page it opens may send the link straight away.
async fn bookmarklet_token(state: &AppState, user_id: i64) -> Result<String, AppError> {
    let mac = bookmarklet_mac(state, user_id).await?;
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Whether `token` is user `user_id`'s [bookmarklet token](bookmarklet_token).
async fn is_bookmarklet_token(
    state: &AppState,
    user_id: i64,
    token: &str,
) -> Result<bool, AppError> {
    let Ok(token) = hex::decode(token) else {
        return Ok(false);
    };
    if token.is_empty() {
        return Ok(false);
    }
    let mac = bookmarklet_mac(state, user_id).await?;
    Ok(mac.verify_slice(&token).is_ok())
}

/// The bookmarklet to drag to the toolbar, and the sites whose works'
/// pages are read. An uploader's carries their bookmarklet token.
async fn bookmarklet(page: Page) -> Result<Response, AppError> {
    let state = page.state();
    let new_url = state
        .config
        .server
        .public_url
        .join("/uploads/new")
        .map_or_else(|_| "/uploads/new".to_owned(), String::from);
    let token = match &page.current.user {
        Some(user) if page.current.can(Permission::Upload) => {
            Some(bookmarklet_token(state, user.id).await?)
        }
        _ => None,
    };
    let query = token
        .as_deref()
        .map_or_else(String::new, |token| format!("token={token}&"));
    let script = format!(
        "javascript:location.href='{new_url}?{query}url='+encodeURIComponent(location.href)\
         +'&ref='+encodeURIComponent(document.referrer)"
    );
    let mut sites: Vec<&moekura_core::sites::Site> = moekura_core::sites::ALL
        .iter()
        .copied()
        .filter(|site| crate::sources::READ.contains(&site.key))
        .collect();
    sites.sort_by_cached_key(|site| site.name.to_lowercase());
    sites.dedup_by_key(|site| site.name);
    let mut response = page.render(
        "upload_bookmarklet.html",
        context! {
            script => script,
            personal => token.is_some(),
            sites => sites.iter().map(|site| context! {
                name => site.name,
                url => site.url,
            }).collect::<Vec<_>>(),
        },
    );
    if token.is_some() {
        // The token is the user's own: keep it out of every cache.
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    Ok(response)
}

/// Makes the user's bookmarklet token anew: a site the bookmarklet was
/// used on saw the old one, which now only fills in the form.
async fn new_bookmarklet_key(page: Page, jar: CookieJar) -> Result<Response, AppError> {
    let user = uploader(&page.current)?;
    moekura_db::secrets::remove(page.state().db.primary(), &bookmarklet_key(user.id)).await?;
    tracing::info!(user = user.name, "bookmarklet key replaced");
    let to = Redirect::to("/uploads/bookmarklet");
    Ok((flash::set(jar, Flash::Saved), to).into_response())
}

/// The link on the upload form, and the page it was found on.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Link<'a> {
    pub url: &'a str,
    pub referer: &'a str,
}

/// What the upload form says besides the link and what went wrong.
#[derive(Default)]
struct FormNotes<'a> {
    /// Scripts send the link straight away: the user's bookmarklet opened
    /// the page with it.
    send_now: bool,
    allowance: Option<&'a Allowance>,
    /// The rules are pointed out to those who've posted little.
    new_uploader: bool,
    /// How many files the link's work has, when that's more than are
    /// taken without asking.
    many_files: Option<usize>,
}

/// The upload form, with the link typed and what went wrong, if
/// anything.
pub(crate) fn new_form(
    page: &Page,
    link: Link<'_>,
    error: Option<&UploadError>,
    status: StatusCode,
    allowance: Option<&Allowance>,
    new_uploader: bool,
) -> Response {
    let notes = FormNotes {
        allowance,
        new_uploader,
        ..FormNotes::default()
    };
    form_page(page, link, error, status, &notes)
}

/// [`new_form`], saying `notes` as well.
fn form_page(
    page: &Page,
    link: Link<'_>,
    error: Option<&UploadError>,
    status: StatusCode,
    notes: &FormNotes<'_>,
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
            send_now => error.is_none() && notes.send_now,
            error => message,
            duplicate_of => duplicate_of,
            many_files => notes.many_files,
            max_link_files => MAX_LINK_FILES,
            max_mb => page.state().media.config().max_upload_mb,
            max_files => MAX_FILES,
            allowance => notes.allowance.map(|a| context! {
                refusal => a.refusal,
                pending_left => a.pending_left,
                today_left => a.today_left,
            }),
            new_uploader => notes.new_uploader,
        },
    )
}

/// The link sent to `/uploads` and the page it's from.
#[derive(Debug, Default)]
struct SentLink {
    url: String,
    referer: String,
    /// "Download all its files" ticked: a work of more than [`MAX_FILES`]
    /// files is taken whole (up to [`MAX_LINK_FILES`]).
    all_files: bool,
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
            name @ ("url" | "ref" | "all_files") => {
                let name = name.to_owned();
                match upload::upload_text(state, field, upload::LINK_FIELD_MAX).await {
                    Ok(text) if name == "url" => link.url = text.trim().to_owned(),
                    Ok(text) if name == "ref" => link.referer = text.trim().to_owned(),
                    Ok(text) => link.all_files = !text.is_empty(),
                    Err(error) => return (link, Err(error)),
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
        new_form(&page, link, Some(&error), error_status(&error), None, false)
    };
    if let Err(error) = upload::check_limits(&state, &page.current).await {
        return Ok(refuse(Link::default(), error));
    }
    let room = match room(&state, &user).await {
        Ok(room) => room,
        Err(error) => return Ok(refuse(Link::default(), error)),
    };
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
    let files = match unpack_archives(&state, files).await {
        Ok(files) => files,
        Err(error) => return Ok(refuse(sent.link(), error)),
    };
    if files.len() > room {
        let error = UploadError::Limit(format!(
            "You may have {MAX_WAITING} files waiting to be posted, so you can send {room} more now."
        ));
        return Ok(refuse(sent.link(), error));
    }
    let id = if !files.is_empty() {
        // A link sent with files says where they're from, if it's a web
        // page.
        let source = if is_web_link(&sent.url) {
            &sent.url
        } else {
            ""
        };
        stage_files(&state, &page.current, user.id, source, &files).await?
    } else if is_web_link(&sent.url) {
        let info = state.sources.lookup_from(&sent.url, &sent.referer).await;
        if let Err(error) = upload::check_found(&sent.url, info.as_deref()) {
            return Ok(refuse(sent.link(), error));
        }
        // A work of many files is only taken whole when asked.
        let count = info.as_deref().map_or(1, |info| work_files(info).len());
        if count > MAX_FILES && !sent.all_files {
            let error = UploadError::Invalid(format!(
                "This work has {count} files. To download them all, upload it again \
                 with the box below ticked."
            ));
            let notes = FormNotes {
                many_files: Some(count),
                ..FormNotes::default()
            };
            return Ok(form_page(
                &page,
                sent.link(),
                Some(&error),
                error_status(&error),
                &notes,
            ));
        }
        let most = if sent.all_files {
            MAX_LINK_FILES
        } else {
            MAX_FILES
        };
        stage_link(
            &state,
            &page.current,
            user.id,
            sent.link(),
            info,
            most.min(room),
        )
        .await?
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

/// `files` with the zip archives among them (but not ugoira) replaced by
/// their files, named `<archive>/<path>`: [`MAX_LINK_FILES`] at most in
/// all, each within the upload size limit.
async fn unpack_archives(
    state: &AppState,
    files: Vec<TempUpload>,
) -> Result<Vec<TempUpload>, UploadError> {
    use moekura_media::archive::{self, Limits};
    use tokio::io::AsyncReadExt;

    let max_mb = state.media.config().max_upload_mb;
    let max_bytes = max_mb * 1024 * 1024;
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let mut head = [0; 4];
        let read = tokio::fs::File::open(file.path())
            .await
            .map_err(|e| UploadError::Internal(format!("reading an upload: {e}")))?
            .read(&mut head)
            .await
            .map_err(|e| UploadError::Internal(format!("reading an upload: {e}")))?;
        let refused =
            |e: archive::ArchiveError| UploadError::Invalid(format!("{}: {e}.", file.name()));
        if !archive::is_zip(&head[..read]) {
            out.push(file);
            continue;
        }
        let path = file.path().to_owned();
        let is_ugoira = tokio::task::spawn_blocking(move || archive::is_ugoira(&path))
            .await
            .map_err(|e| UploadError::Internal(e.to_string()))?;
        // A damaged zip is left to say so when it's stored.
        if is_ugoira.unwrap_or(true) {
            out.push(file);
            continue;
        }
        let dir = ScratchDir(state.work_dir.join(format!(
            "unpacked-{}",
            &hex::encode(moekura_core::tokens::NewToken::generate().hash)[..24]
        )));
        tokio::fs::create_dir_all(&dir.0)
            .await
            .map_err(|e| UploadError::Internal(format!("unpacking: {e}")))?;
        let limits = Limits {
            max_files: MAX_LINK_FILES,
            max_total_bytes: max_bytes.saturating_mul(MAX_FILES as u64),
        };
        // Unpacking goes on when the request ends early (the connection
        // closed, the time ran out), so the directory goes with it, and
        // is removed with the task's result if nobody takes that.
        let path = file.path().to_owned();
        let (unpacked, dir) = tokio::task::spawn_blocking(move || {
            let unpacked = archive::unpack(&path, &dir.0, limits);
            (unpacked, dir)
        })
        .await
        .map_err(|e| UploadError::Internal(e.to_string()))?;
        for entry in unpacked.map_err(refused)? {
            let name = format!("{}/{}", file.name(), entry.name);
            let size = tokio::fs::metadata(&entry.path)
                .await
                .map_err(|e| UploadError::Internal(format!("unpacking: {e}")))?
                .len();
            if size > max_bytes {
                return Err(UploadError::Invalid(format!(
                    "{name} is larger than {max_mb} MB."
                )));
            }
            out.push(TempUpload::adopt(&state.work_dir, &entry.path, &name).await?);
        }
        drop(dir);
        if out.len() > MAX_LINK_FILES {
            return Err(UploadError::Invalid(format!(
                "Upload at most {MAX_LINK_FILES} files at once, archives' files included."
            )));
        }
    }
    Ok(out)
}

/// A scratch directory in the work directory (an archive unpacked, a
/// searched file's frames), removed when dropped: when the request ends
/// early too.
pub(crate) struct ScratchDir(pub PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the work directory's scratch files for uploads (`upload-…`),
/// unpacked archives (`unpacked-…`), image searches (`search-…`) and
/// profile pictures (`avatar-…`, `banner-…`) start with.
const SCRATCH_PREFIXES: [&str; 5] = ["upload-", "unpacked-", "search-", "avatar-", "banner-"];

/// Removes scratch files left in the work directory by requests that
/// never finished (the server stopped meanwhile). Only those older than
/// [`STALE_AFTER`] go: other processes may share the directory, and
/// younger ones may be in use. Runs at startup and every hour.
pub async fn sweep_work_dir(state: &AppState) {
    let dir = state.work_dir.clone();
    match tokio::task::spawn_blocking(move || sweep(&dir, STALE_AFTER)).await {
        Ok(0) => {}
        Ok(removed) => tracing::info!(removed, "removed stale upload scratch files"),
        Err(error) => tracing::warn!(%error, "could not sweep the work directory"),
    }
}

/// Removes the scratch files in `dir` unchanged for longer than `max_age`,
/// returning how many.
fn sweep(dir: &FsPath, max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let scratch = name
            .to_str()
            .is_some_and(|name| SCRATCH_PREFIXES.iter().any(|p| name.starts_with(p)));
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|changed| changed.elapsed().is_ok_and(|age| age > max_age));
        if !(scratch && stale) {
            continue;
        }
        let path = entry.path();
        let gone = if entry.file_type().is_ok_and(|t| t.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed += usize::from(gone.is_ok());
    }
    removed
}

/// Makes an upload of files sent from `source` by `current`, storing each.
/// Returns the upload's id.
async fn stage_files(
    state: &AppState,
    current: &CurrentUser,
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
            let prepared = upload::prepare(state, Some(current), file).await?;
            Ok((prepared, upload::phash(state, file).await))
        }
        .await;
        match prepared {
            Ok((prepared, hash)) => {
                let mut tx = db.begin().await?;
                if let Err(error) =
                    upload::check_staged(state, &mut tx, uploader_id, &prepared).await
                {
                    drop(tx);
                    upload::forget_original(state, &prepared).await;
                    let (message, duplicate_of) = failure(&error);
                    staged_uploads::create_failed(db, slot, &message, duplicate_of).await?;
                    continue;
                }
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

/// The links of the work's files that `info` lists, each once.
fn work_files(info: &SourceInfo) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    info.files
        .iter()
        .map(String::as_str)
        .filter(|file| seen.insert(*file))
        .collect()
}

/// Makes an upload of the files at `link` (at most `most`, counted again
/// as it's made) for `current`: a work's files
/// when a source strategy read its page (or the page it was found on,
/// for a bare file) and said `info`, else the link itself. Each file's
/// source is its [canonical one](upload::file_source). They're
/// downloaded in the background; this waits a little for them. Returns
/// the upload's id.
async fn stage_link(
    state: &AppState,
    current: &CurrentUser,
    uploader_id: i64,
    link: Link<'_>,
    info: Option<Arc<SourceInfo>>,
    most: usize,
) -> Result<i64, AppError> {
    let url = link.url;
    let (files, source) = match info.as_deref() {
        Some(info) if !info.files.is_empty() => (
            work_files(info)
                .into_iter()
                .take(most)
                .map(str::to_owned)
                .collect(),
            info.page_url.clone(),
        ),
        _ => (vec![url.to_owned()], url.to_owned()),
    };
    let source: String = source.chars().take(SOURCE_MAX_LEN).collect();
    let mut tx = state.db.primary().begin().await?;
    if let Err(error) = upload::check_room(&mut tx, uploader_id, files.len()).await {
        return Err(match error {
            UploadError::Limit(message) => AppError::Blocked(message),
            error => AppError::Internal(error.to_string()),
        });
    }
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
    // Together they may come to as much as files sent at once.
    let budget =
        (state.media.config().max_upload_mb * 1024 * 1024).saturating_mul(MAX_FILES as u64);
    let (first, first_done) = tokio::sync::oneshot::channel();
    let downloads = download_pending(
        state.clone(),
        current.clone(),
        id,
        uploader_id,
        info,
        budget,
        first,
    );
    tokio::spawn(downloads);
    // The page follows the rest (and one that takes long).
    let _ = tokio::time::timeout(WAIT_FOR_DOWNLOADS, first_done).await;
    Ok(id)
}

/// Downloads and stores upload `upload_id`'s pending files for `current`,
/// one at a time, saying on `first` when the first is done. `info` is
/// what the link's page said, if a strategy read it. Once `budget` bytes
/// were received (by downloads that failed too), the rest fail.
async fn download_pending(
    state: AppState,
    current: CurrentUser,
    upload_id: i64,
    uploader_id: i64,
    info: Option<Arc<SourceInfo>>,
    budget: u64,
    first: tokio::sync::oneshot::Sender<()>,
) {
    let mut first = Some(first);
    let db = state.db.primary();
    let pending = match staged_uploads::of_upload(db, upload_id).await {
        Ok(files) => files,
        Err(error) => {
            tracing::error!(upload_id, %error, "upload's files not downloaded");
            return;
        }
    };
    let mine = user_turns(uploader_id);
    let received = Arc::new(AtomicU64::new(0));
    for file in pending.iter().filter(|f| f.status == Status::Pending) {
        let url = file.file_url.as_deref().unwrap_or_default();
        // Those past the budget fail without waiting for a turn.
        let spent = received.load(Ordering::Relaxed) >= budget;
        let _turn = if spent {
            None
        } else {
            Some(take_turn(db, upload_id, &mine).await)
        };
        let done = async {
            // Given up on while it waited (see `find_upload`).
            if !staged_uploads::claim(db, file.id).await? {
                return Ok(());
            }
            let fetched = async {
                if spent {
                    return Err(UploadError::Invalid(format!(
                        "The upload's files reached the {} MB it may download, \
                         so this one wasn't downloaded.",
                        budget.div_ceil(1024 * 1024)
                    )));
                }
                let temp =
                    upload::download_counted(&state, url, info.as_deref(), Some(&received)).await?;
                let prepared = upload::prepare(&state, Some(&current), &temp).await?;
                Ok::<_, UploadError>((prepared, upload::phash(&state, &temp).await))
            }
            .await;
            match fetched {
                Ok((prepared, hash)) => {
                    let recorded = async {
                        let mut tx = db.begin().await?;
                        upload::keep_original(&state, &mut tx, &prepared).await?;
                        if !staged_uploads::stored(&mut *tx, file.id, prepared.stored(hash)).await?
                        {
                            return Ok(false);
                        }
                        crate::suggestions::queue_staged(&state, &mut tx, file.id).await?;
                        tx.commit().await?;
                        Ok::<_, UploadError>(true)
                    }
                    .await;
                    if matches!(recorded, Ok(true)) {
                        return Ok(());
                    }
                    // Nothing records the file: it was given up on while it
                    // downloaded (see find_upload), or recording it failed.
                    upload::forget_original(&state, &prepared).await;
                    match recorded {
                        Err(error) => {
                            staged_uploads::failed(db, file.id, &failure(&error).0, None).await
                        }
                        Ok(_) => Ok(()),
                    }
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
        if let Some(first) = first.take() {
            let _ = first.send(());
        }
    }
}

/// User `user_id`'s turns to download (see [`MAX_USER_DOWNLOADS`]).
fn user_turns(user_id: i64) -> Arc<Semaphore> {
    let mut users = USER_DOWNLOADS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // Users whose downloads have all ended are forgotten.
    users.retain(|_, turns| Arc::strong_count(turns) > 1);
    users
        .entry(user_id)
        .or_insert_with(|| Arc::new(Semaphore::new(MAX_USER_DOWNLOADS)))
        .clone()
}

/// Waits for a turn to download: one of the uploader's turns `mine`, then
/// one of [`MAX_DOWNLOADS`]. Upload `upload_id`'s files waiting are
/// marked as still wanted meanwhile, so a long wait doesn't have them
/// taken for abandoned.
async fn take_turn(
    db: &sqlx::PgPool,
    upload_id: i64,
    mine: &Arc<Semaphore>,
) -> (OwnedSemaphorePermit, SemaphorePermit<'static>) {
    let turn = async {
        let closed = "the semaphores are never closed";
        let own = mine.clone().acquire_owned().await.expect(closed);
        (own, DOWNLOADS.acquire().await.expect(closed))
    };
    // One wait throughout, which keeps its place in the queues.
    let mut turn = std::pin::pin!(turn);
    loop {
        if let Err(error) = staged_uploads::still_pending(db, upload_id).await {
            tracing::warn!(upload_id, %error, "upload's files waiting not marked as wanted");
        }
        if let Ok(turn) = tokio::time::timeout(STILL_WAITING, &mut turn).await {
            return turn;
        }
    }
}

/// An upload's files' progress, for scripts following it: how many are
/// still downloading, and each one's status and page.
async fn status(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let state = page.state();
    let (upload, files) = find_upload(state, &page.current, id, Access::View).await?;
    let pending = files.iter().filter(|f| f.status == Status::Pending).count();
    Ok(axum::Json(serde_json::json!({
        "pending": pending,
        "files": files.iter().map(|f| serde_json::json!({
            "id": f.id,
            "status": f.status.as_str(),
            "url": format!("/uploads/{}/assets/{}", upload.id, f.id),
        })).collect::<Vec<_>>(),
    }))
    .into_response())
}

/// What's wanted of an upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// To look at it: its uploader, or a moderator.
    View,
    /// To post its files: its uploader only.
    Post,
}

/// Whether `current` may see others' uploads.
fn sees_all_uploads(current: &CurrentUser) -> bool {
    current.can(Permission::BanUsers)
}

/// Upload `id`, if `current` may have it for `access`, and its files,
/// giving up on downloads that were abandoned.
async fn find_upload(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    access: Access,
) -> Result<(Upload, Vec<Staged>), AppError> {
    let user = uploader(current)?;
    let db = state.db.primary();
    let upload = staged_uploads::upload_by_id(db, id)
        .await?
        .filter(|u| {
            u.uploader_id == user.id || (access == Access::View && sees_all_uploads(current))
        })
        .ok_or(AppError::NotFound)?;
    let abandoned = "The download stopped before it finished; upload the link again.";
    staged_uploads::fail_abandoned(db, id, ABANDONED_AFTER, abandoned).await?;
    let files = staged_uploads::of_upload(db, id).await?;
    Ok((upload, files))
}

async fn show(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    let state = page.state();
    let (upload, files) = find_upload(state, &page.current, id, Access::View).await?;
    // A file that's already a post is that post.
    if let [file] = files.as_slice()
        && let Some(post) = file.duplicate_of
        && posts::by_id(state.db.primary(), post)
            .await?
            .is_some_and(|p| crate::posts::visibility(&page.current).allows(&p))
    {
        let to = Redirect::to(&format!("/posts/{post}"));
        return Ok((flash::set(jar, Flash::Duplicate), to).into_response());
    }
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
            ready => files.iter().filter(|f| f.status == Status::Ready).count(),
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
        // Only a web page is linked to.
        source_link => is_web_link(&upload.source),
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
    translated_title: String,
    translated_description: String,
    /// "Upload for approval", ticked.
    for_approval: String,
    /// A suggested tag clicked on (without scripts, which add it to the
    /// tags box): added to the form, which is shown again.
    add: String,
    /// The suggested rating, clicked on likewise.
    suggested_rating: String,
    /// "Check again" for suggestions: the form is shown again.
    refresh: String,
    /// "Fetch source data": the source is looked up again and the form
    /// shown again.
    fetch_source: String,
}

impl AssetFields {
    /// The form as it starts: the file's source, and, when a source
    /// strategy reads its page, the artist's tag (when they have an
    /// entry) and commentary.
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
            match crate::sources::artists_for(state.db.primary(), &info).await {
                Ok(artists) => {
                    for artist in artists {
                        fields.tags.push_str(&artist.name);
                        fields.tags.push(' ');
                    }
                }
                Err(error) => tracing::warn!(%error, "the source's artists weren't looked up"),
            }
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
            translated_title: self.translated_title.trim().to_owned(),
            translated_description: self.translated_description.trim().to_owned(),
            for_approval: !self.for_approval.is_empty(),
            ..UploadFields::default()
        }
    }
}

/// Upload `id`'s file `file_id`, if `current` may have it for `access`,
/// with the upload's files and its place among them.
async fn find_file(
    state: &AppState,
    current: &CurrentUser,
    (id, file_id): (i64, i64),
    access: Access,
) -> Result<(Upload, Vec<Staged>, usize), AppError> {
    let (upload, files) = find_upload(state, current, id, access).await?;
    let index = files
        .iter()
        .position(|f| f.id == file_id)
        .ok_or(AppError::NotFound)?;
    Ok((upload, files, index))
}

async fn asset(page: Page, Path(ids): Path<(i64, i64)>) -> Result<Response, AppError> {
    let state = page.state();
    let (upload, files, index) = find_file(state, &page.current, ids, Access::View).await?;
    let fields = AssetFields::for_file(state, &upload, &files[index]).await;
    asset_page(&page, &upload, &files, index, &fields, None).await
}

async fn post_asset(
    page: Page,
    Path(ids): Path<(i64, i64)>,
    Form(fields): Form<AssetFields>,
) -> Result<Response, AppError> {
    let state = page.state();
    let (upload, files, index) = find_file(state, &page.current, ids, Access::Post).await?;
    let file_id = files[index].id;
    // A suggestion taken, or a check for them or the source, without
    // scripts: the form again, with the suggestion in it, rather than a
    // post.
    if !(fields.add.is_empty()
        && fields.suggested_rating.is_empty()
        && fields.refresh.is_empty()
        && fields.fetch_source.is_empty())
    {
        let mut fields = fields;
        if !fields.fetch_source.is_empty() && is_web_link(fields.source.trim()) {
            crate::sources::lookup_for_panel(
                state,
                &page.current,
                None,
                fields.source.trim(),
                true,
            )
            .await?;
        }
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
async fn asset_suggestions(page: Page, Path(ids): Path<(i64, i64)>) -> Result<Response, AppError> {
    let state = page.state();
    let (_, files, index) = find_file(state, &page.current, ids, Access::Post).await?;
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

/// What the post form shows of what a source said: the site and page,
/// the artist (their entries here, or a link starting one) and profiles,
/// the site's tags (linked to this site's where known), and when the work
/// was published and changed.
async fn source_data(state: &AppState, info: &SourceInfo) -> Result<Value, AppError> {
    let db = state.db.primary();
    let artists = crate::sources::artists_for(db, info).await?;
    let new_artist = if artists.is_empty() {
        crate::artists::unknown_artist(info)
    } else {
        None
    };
    let translated = crate::sources::translated_tags(db, info).await?;
    let categories = moekura_db::tags::categories(db).await?;
    let category = |id: i16| {
        categories
            .iter()
            .find(|c| c.id == id)
            .map_or("general", |c| c.name.as_str())
    };
    let tags: Vec<Value> = info
        .tags
        .iter()
        .map(|tag| {
            let local = translated.iter().find(|(_, from)| *from == tag.name);
            context! {
                name => tag.name,
                translation => tag.translation,
                local => local.map(|(t, _)| context! {
                    name => t.name,
                    category => category(t.category_id),
                    url => url_value(&format!(
                        "/posts?tags={}",
                        url::form_urlencoded::byte_serialize(t.name.as_bytes()).collect::<String>()
                    )),
                }),
            }
        })
        .collect();
    Ok(context! {
        site => info.site,
        // Only web pages are linked to.
        page_url => is_web_link(&info.page_url).then(|| url_value(&info.page_url)),
        artist_name => info.artist_name,
        artist_account => info.artist_account,
        profiles => info
            .profile_urls
            .iter()
            .filter(|u| is_web_link(u))
            .map(|u| url_value(u))
            .collect::<Vec<_>>(),
        artists => artists.iter().map(|a| context! {
            name => a.name,
            url => url_value(&format!("/artists/{}", a.id)),
        }).collect::<Vec<_>>(),
        new_artist_url => new_artist.map(|u| url_value(&u.new_url)),
        tags => tags,
        published => info.published_at.map(crate::dates::day),
        updated => info
            .updated_at
            .filter(|u| Some(*u) != info.published_at)
            .map(crate::dates::day),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SourceDataQuery {
    url: String,
    /// Look the source up again rather than reuse what was found (see
    /// [`crate::sources::may_refresh`]).
    refresh: String,
}

/// The post form's source panel for `url`, for scripts fetching it again
/// after the source was changed. Empty when no strategy reads it.
async fn source_data_fragment(
    page: Page,
    Query(query): Query<SourceDataQuery>,
) -> Result<Response, AppError> {
    uploader(&page.current)?;
    let state = page.state();
    let info = if !is_web_link(query.url.trim()) {
        None
    } else {
        let refresh = !query.refresh.is_empty();
        crate::sources::lookup_for_page(state, &page.current, None, &query.url, refresh).await?
    };
    let source = match info {
        Some(info) => Some(source_data(state, &info).await?),
        None => None,
    };
    Ok(page.render(
        "upload_source.html",
        context! { source => source, source_url => query.url.trim() },
    ))
}

/// Uploaders with fewer posts than this are shown the rules.
const NEW_UPLOADER_POSTS: i64 = 10;
/// The wiki page shown as the post form's help.
const HELP_PAGE: &str = "help:upload_notice";

/// Whether `user` is new to uploading (shown the rules), and the upload
/// help ([`HELP_PAGE`]), saying whether it changed since their last
/// post.
async fn guidance(state: &AppState, user: &User) -> Result<(bool, Option<Value>), AppError> {
    let db = state.db.primary();
    let (posts, last) = moekura_db::posts::uploaded_by(db, user.id).await?;
    let help = moekura_db::wiki::by_title(db, HELP_PAGE)
        .await?
        .map(|page| {
            context! {
                html => Value::from_safe_string(moekura_core::markup::render(&page.body)),
                url => url_value(&moekura_core::markup::wiki_url(HELP_PAGE)),
                changed => last.is_none_or(|last| page.updated_at > last),
            }
        });
    Ok((posts < NEW_UPLOADER_POSTS, help))
}

/// Posts from the same source shown on the post form.
const RELATED_SHOWN: u32 = 8;
/// Most sources looked for.
const RELATED_SOURCES: usize = 10;

/// Posts already here from the same source as an upload's files: whose
/// source starts with the work's page (`page_url`) or with one of the
/// files' sources (a Pixiv work's images). With the search finding them
/// all and how many it finds. `None` when there are none.
async fn related_by_source(
    page: &Page,
    files: &[Staged],
    page_url: Option<&str>,
) -> Result<Option<Value>, AppError> {
    use moekura_core::search::Query as SearchQuery;
    use moekura_db::search::{Count, PageRef, Plan};

    let mut sources: Vec<&str> = page_url.into_iter().collect();
    for file in files {
        if is_web_link(&file.source) && !sources.contains(&file.source.as_str()) {
            sources.push(&file.source);
        }
    }
    sources.truncate(RELATED_SOURCES);
    // Terms are split at spaces, so links with any can't be searched.
    sources.retain(|s| !s.contains(char::is_whitespace));
    let terms: Vec<String> = sources.iter().map(|s| format!("source:{s}")).collect();
    let search = match terms.as_slice() {
        [] => return Ok(None),
        [one] => one.clone(),
        many => many
            .iter()
            .map(|t| format!("~{t}"))
            .collect::<Vec<_>>()
            .join(" "),
    };
    let Ok(mut query) = SearchQuery::parse(&search) else {
        return Ok(None);
    };
    query.limit = Some(RELATED_SHOWN);
    let state = page.state();
    let db = state.db.primary();
    let visible = crate::posts::visibility(&page.current);
    let Ok(mut plan) = Plan::resolve(db, &query, &visible, &state.search_config()).await else {
        return Ok(None);
    };
    plan.exclude(&crate::blacklist::exclusions(state, db, &page.current).await?);
    let (Ok(ids), Ok(count)) = (plan.ids(db, PageRef::default()).await, plan.count(db).await)
    else {
        return Ok(None);
    };
    if ids.is_empty() {
        return Ok(None);
    }
    let count = match count {
        Count::Exact(n) | Count::About(n) | Count::AtLeast(n) => n,
    };
    let cards: Vec<Value> = crate::posts::grid(page, db, &ids, None)
        .await?
        .into_iter()
        .map(|(_, card)| card)
        .collect();
    Ok(Some(context! {
        posts => cards,
        count => count,
        search_url => url_value(&format!(
            "/posts?tags={}",
            url::form_urlencoded::byte_serialize(search.as_bytes()).collect::<String>()
        )),
    }))
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
    // Close matches are warned about; less alike ones are behind a
    // button.
    let (similar, less_similar) = if file.status == Status::Ready && file.post_id.is_none() {
        let hash = file.phash.map(|h| h as u64);
        let found = upload::lookalikes(
            state,
            &page.current,
            hash,
            None,
            crate::image_search::MAX_DISTANCE,
            upload::LOOKALIKES_SHOWN * 2,
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
        let ids: Vec<i64> = found.iter().map(|(id, _)| *id).collect();
        let cards = crate::posts::grid(page, state.db.primary(), &ids, None).await?;
        let mut close = Vec::new();
        let mut far = Vec::new();
        for (id, distance) in found {
            let Some((_, card)) = cards.iter().find(|(card_id, _)| *card_id == id) else {
                continue;
            };
            let similarity = crate::image_search::Match {
                post_id: id,
                distance,
            }
            .similarity();
            let shown = context! { card => card, similarity => similarity };
            if distance <= moekura_db::media::SIMILAR_MAX_DISTANCE {
                close.push(shown);
            } else {
                far.push(shown);
            }
        }
        (close, far)
    } else {
        (Vec::new(), Vec::new())
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
    // Moderators may look at others' uploads, but not post them.
    let mine = page
        .current
        .user
        .as_ref()
        .is_some_and(|u| u.id == upload.uploader_id);
    let uploader_name = if mine {
        None
    } else {
        moekura_db::users::by_id(state.db.primary(), upload.uploader_id)
            .await?
            .map(|u| u.name)
    };
    let open = file.status == Status::Ready && file.post_id.is_none() && mine;
    let warnings = if open {
        Some(warnings(page, file, &fields.source).await?)
    } else {
        None
    };
    let source_url = fields.source.trim();
    let info = if open && is_web_link(source_url) {
        crate::sources::lookup_for_panel(state, &page.current, None, source_url, false).await?
    } else {
        None
    };
    let source = match &info {
        Some(info) => Some(source_data(state, info).await?),
        None => None,
    };
    // Users whose posts wait for approval see how many more they may
    // upload; others may choose to have theirs wait.
    let queued = upload::queued(state, &page.current);
    let allowance = if open && queued {
        Some(upload::allowance(state, &page.current).await?)
    } else {
        None
    };
    let (new_uploader, help) = match &page.current.user {
        Some(user) if open => guidance(state, user).await?,
        _ => (false, None),
    };
    let related = if open {
        let page_url = info
            .as_deref()
            .map(|i| i.page_url.as_str())
            .or_else(|| (!upload.source.is_empty()).then_some(upload.source.as_str()));
        related_by_source(page, files, page_url).await?
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
            mine => mine,
            uploader_name => uploader_name,
            similar => similar,
            less_similar => less_similar,
            related => related,
            warnings => warnings,
            source => source,
            source_url => source_url,
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
                translated_title => fields.translated_title,
                translated_description => fields.translated_description,
                for_approval => !fields.for_approval.is_empty(),
            },
            can_choose_approval => !queued,
            show_rules => new_uploader && !state.site.get().settings.rules.trim().is_empty(),
            help => help,
            allowance => allowance.map(|a| context! {
                pending_left => a.pending_left,
                today_left => a.today_left,
            }),
            error => message,
            duplicate_of => duplicate_of,
            refresh => (file.status == Status::Pending).then_some(REFRESH_SECS),
        },
    ))
}

/// The file types an upload can be.
const FILE_TYPES: &[&str] = &[
    "jpeg", "png", "gif", "webp", "avif", "jxl", "mp4", "webm", "ugoira",
];

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IndexQuery {
    page: Option<i64>,
    /// `pending`, `ready` or `failed`.
    status: String,
    /// `yes` or `no`.
    posted: String,
    /// A file type.
    #[serde(rename = "type")]
    file_type: String,
    /// The start of the source or link (`*` matches anything).
    source: String,
    /// For moderators: whose files, by name; blank for everyone's. Others
    /// only ever see their own.
    user: Option<String>,
}

impl IndexQuery {
    /// The query for page `number`, for links.
    fn url(&self, number: i64) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in [
            ("status", &self.status),
            ("posted", &self.posted),
            ("type", &self.file_type),
            ("source", &self.source),
        ] {
            if !value.is_empty() {
                query.append_pair(name, value);
            }
        }
        if let Some(user) = &self.user {
            query.append_pair("user", user);
        }
        query.append_pair("page", &number.to_string());
        format!("/uploads?{}", query.finish())
    }
}

/// The user's uploaded files, newest first, or (for moderators) anyone's;
/// filtered by status, whether they were posted, file type and source.
async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    let user = uploader(&page.current)?;
    let state = page.state();
    let db = state.db.primary();
    let everyone = sees_all_uploads(&page.current);
    let uploader_id = match query.user.as_deref().map(str::trim) {
        Some("") if everyone => None,
        // A name nobody has matches no files.
        Some(name) if everyone => Some(
            moekura_db::users::by_name(db, name)
                .await?
                .map_or(-1, |u| u.id),
        ),
        _ => Some(user.id),
    };
    let filter = staged_uploads::Filter {
        uploader_id,
        status: match query.status.as_str() {
            "pending" => Some(Status::Pending),
            "ready" => Some(Status::Ready),
            "failed" => Some(Status::Failed),
            _ => None,
        },
        posted: match query.posted.as_str() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        },
        media_type: FILE_TYPES.iter().copied().find(|t| *t == query.file_type),
        source: Some(query.source.trim()).filter(|s| !s.is_empty()),
    };
    let number = query.page.unwrap_or(1).clamp(1, 10_000);
    let mut files =
        staged_uploads::search(db, &filter, (number - 1) * PAGE_SIZE, PAGE_SIZE + 1).await?;
    let more = files.len() > PAGE_SIZE as usize;
    files.truncate(PAGE_SIZE as usize);
    let list_url = |n: i64| url_value(&query.url(n));
    Ok(page.render(
        "uploads.html",
        context! {
            files => file_cards(&page, &files).await?,
            previous_url => (number > 1).then(|| list_url(number - 1)),
            next_url => more.then(|| list_url(number + 1)),
            filter => context! {
                status => query.status,
                posted => query.posted,
                file_type => query.file_type,
                source => query.source,
                user => query.user,
                filtered => filter != staged_uploads::Filter {
                    uploader_id: Some(user.id),
                    ..staged_uploads::Filter::default()
                },
            },
            file_types => FILE_TYPES,
            sees_all => everyone,
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
    use crate::test_support::{TestApp, current_user, fixture, session_for, test_state};

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

        // The bookmarklet opens the form with the link, which scripts send
        // when it carries the user's token.
        let token = bookmarklet_token(&state, current_user(&state, &alice).await.user.unwrap().id)
            .await
            .unwrap();
        let opened = |token: &str| {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("token", token)
                .append_pair("url", &file)
                .append_pair("ref", &work)
                .finish();
            format!("/uploads/new?{query}")
        };
        let form = app.get(&opened(&token), Some(&alice)).await;
        assert!(form.body.contains("data-upload-send-now"), "{}", form.body);
        assert!(form.body.contains("name=\"ref\""), "{}", form.body);
        let plain = app.get("/uploads/new", Some(&alice)).await;
        assert!(!plain.body.contains("data-upload-send-now"));
        // Any other site can link here with a link, which then waits for
        // the button.
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let bobs = bookmarklet_token(&state, current_user(&state, &bob).await.user.unwrap().id)
            .await
            .unwrap();
        for token in ["", "00", &bobs, &token[..32]] {
            let form = app.get(&opened(token), Some(&alice)).await;
            assert_eq!(form.status, StatusCode::OK);
            assert!(!form.body.contains("data-upload-send-now"), "{token}");
            assert!(
                form.body
                    .contains(&format!("value=\"http:&#x2f;&#x2f;{addr}"))
            );
        }
        // A new key stops the old bookmarklet sending by itself, and only
        // the user's own.
        let renewed = app.post("/uploads/bookmarklet", Some(&alice), &[]).await;
        assert_eq!(renewed.status, StatusCode::SEE_OTHER);
        assert_eq!(renewed.location.as_deref(), Some("/uploads/bookmarklet"));
        let old = app.get(&opened(&token), Some(&alice)).await;
        assert!(!old.body.contains("data-upload-send-now"));
        let new = bookmarklet_token(&state, current_user(&state, &alice).await.user.unwrap().id)
            .await
            .unwrap();
        assert_ne!(new, token);
        let form = app.get(&opened(&new), Some(&alice)).await;
        assert!(form.body.contains("data-upload-send-now"));
        let form = app.get(&opened(&bobs), Some(&bob)).await;
        assert!(form.body.contains("data-upload-send-now"));

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
        let (app, state) = app(&pool).await;
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
        // Logged out, it carries no token, and says to log in first.
        assert!(!page.body.contains("token="), "{}", page.body);
        assert!(page.body.contains("Log in before"), "{}", page.body);
        assert!(!page.body.contains("Make a new key"), "{}", page.body);
        let refused = app.post("/uploads/bookmarklet", None, &[]).await;
        assert_ne!(refused.status, StatusCode::SEE_OTHER);

        // An uploader's carries theirs, and is kept out of caches.
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let page = app.get("/uploads/bookmarklet", Some(&alice)).await;
        let token = bookmarklet_token(&state, current_user(&state, &alice).await.user.unwrap().id)
            .await
            .unwrap();
        assert!(
            page.body
                .contains(&format!("&#x2f;uploads&#x2f;new?token={token}&amp;url=")),
            "{}",
            page.body
        );
        // The sites it's used on see the key, so a new one can be made.
        assert!(
            page.body.contains("action=\"/uploads/bookmarklet\""),
            "{}",
            page.body
        );
        assert!(page.body.contains("Make a new key"), "{}", page.body);
        let request = axum::http::Request::get("/uploads/bookmarklet")
            .header(
                axum::http::header::COOKIE,
                format!("{}={alice}", crate::auth::SESSION_COOKIE),
            )
            .body(axum::body::Body::empty())
            .unwrap();
        let response = app.raw(request).await;
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_post_form_warns_about_the_file(pool: PgPool) {
        // Originals kept as uploaded, so a file differing from a post only
        // in its metadata isn't that post.
        let mut config = crate::test_support::test_config();
        config.media.strip_metadata = moekura_core::config::StripMetadata::Off;
        let state = crate::test_support::test_state_with(&pool, config).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let routes = routes(max)
            .merge(upload::routes(max))
            .merge(crate::posts::routes());
        let app = TestApp::new(state, routes);
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
    async fn the_post_form_shows_what_the_source_says(pool: PgPool) {
        let png = fixture::png(48, 30);
        let origin = Router::new().route("/1.png", get(move || async move { png }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let mut state = test_state(&pool).await;
        state.fetcher = crate::fetch::Fetcher::new(Duration::from_secs(10), true);
        state.sources = Arc::new(crate::sources::Sources::new(
            true,
            moekura_core::config::SourcesConfig::default(),
        ));
        let work = format!("http://{addr}/work");
        state.sources.remember(
            &work,
            SourceInfo {
                site: "Example",
                page_url: work.clone(),
                files: vec![format!("http://{addr}/1.png")],
                artist_name: Some("Cat Artist".into()),
                artist_account: Some("catart".into()),
                profile_urls: vec!["https://example.com/catart".into()],
                tags: vec![crate::sources::SourceTag {
                    name: "猫".into(),
                    translation: Some("cat".into()),
                }],
                title: "Neko".into(),
                published_at: Some(time::macros::datetime!(2024-05-01 12:00 UTC)),
                ..SourceInfo::default()
            },
        );
        let max = 10 * 1024 * 1024;
        let routes = routes(max)
            .merge(upload::routes(max))
            .merge(crate::artists::routes())
            .merge(crate::posts::routes());
        let app = TestApp::new(state.clone(), routes);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;

        // An artist entry for the profile, and an earlier post of the work.
        let created = app
            .post_form(
                "/artists",
                Some(&alice),
                &[],
                "name=cat_artist&urls=https%3A%2F%2Fexample.com%2Fcatart",
            )
            .await;
        assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
        let earlier = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &[
                    ("rating", "g".to_owned()),
                    ("tags", "cat".to_owned()),
                    ("source", work.clone()),
                ],
                Some(("a.png", &fixture::png(40, 30))),
            )
            .await;
        let earlier = post_in(earlier.location.as_deref());
        sqlx::query(
            "INSERT INTO wiki_pages (title, body) VALUES ('help:upload_notice', 'Tag it *well*.')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let sent = app
            .post_multipart("/uploads", Some(&alice), &[("url", work.clone())], None)
            .await;
        let upload = upload_in(sent.location.as_deref());
        let form = app
            .get(&format!("/uploads/{upload}"), Some(&alice))
            .await
            .body;
        // The artist's tag is in the tags box, and the panel says who,
        // what the site tagged it and when it was published.
        assert!(form.contains(">cat_artist </textarea>"), "{form}");
        assert!(form.contains(">cat_artist</a>"), "{form}");
        assert!(form.contains("https://example.com/catart"), "{form}");
        assert!(
            form.contains(">cat</a> <span class=\"hint\">猫</span>"),
            "{form}"
        );
        assert!(form.contains("2024-05-01"), "{form}");
        assert!(form.contains("Fetch source data"), "{form}");
        // Posts already from the same source.
        assert!(form.contains("Related posts"), "{form}");
        assert!(
            form.contains("1 other post</a> from the same source"),
            "{form}"
        );
        assert!(form.contains(&format!("href=\"/posts/{earlier}")), "{form}");
        // Members' posts are active: they may choose to have it approved.
        assert!(form.contains("name=\"for_approval\""), "{form}");
        // The help, new to them since their last post.
        assert!(form.contains("<p>Tag it *well*.</p>"), "{form}");
        assert!(
            form.contains("This has changed since your last upload."),
            "{form}"
        );

        // Scripts fetch the panel again (`refresh=1` would ask the site).
        let query: String = url::form_urlencoded::byte_serialize(work.as_bytes()).collect();
        let panel = app
            .get(&format!("/uploads/source-data?url={query}"), Some(&alice))
            .await
            .body;
        assert!(panel.contains(">cat_artist</a>"), "{panel}");
        assert!(!panel.contains("<html"), "{panel}");

        // Posted with a translation, for approval.
        let file = files_of(&pool, upload).await[0].id;
        let posted = app
            .post_form(
                &format!("/uploads/{upload}/assets/{file}"),
                Some(&alice),
                &[],
                "rating=g&tags=cat_artist&commentary_title=Neko&translated_title=Cat&for_approval=1",
            )
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let post = moekura_db::posts::by_id(&pool, post_in(posted.location.as_deref()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(post.status, moekura_core::posts::PostStatus::Pending);
        let commentary = moekura_db::artist_commentaries::for_post(&pool, post.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                commentary.texts.original_title.as_str(),
                commentary.texts.translated_title.as_str()
            ),
            ("Neko", "Cat")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn look_alikes_say_how_alike(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let mut posts = Vec::new();
        for (name, size) in [("a.png", 40), ("b.png", 44)] {
            let posted = app
                .post_multipart(
                    "/upload",
                    Some(&alice),
                    &[("rating", "g".to_owned())],
                    Some((name, &fixture::png(size, 30))),
                )
                .await;
            posts.push(post_in(posted.location.as_deref()));
        }
        let png = fixture::png(48, 30);
        for post in &posts {
            crate::test_support::hash_like(&state, &pool, *post, &png).await;
        }
        // The second differs in 10 of the 64 bits.
        sqlx::query("UPDATE media_assets SET phash = phash # 1023 WHERE post_id = $1")
            .bind(posts[1])
            .execute(&pool)
            .await
            .unwrap();
        let sent = app
            .post_multipart("/uploads", Some(&alice), &[], Some(("c.png", &png)))
            .await;
        let form = app
            .get(
                &format!("/uploads/{}", upload_in(sent.location.as_deref())),
                Some(&alice),
            )
            .await
            .body;
        assert!(form.contains("100.0% alike"), "{form}");
        assert!(form.contains("Show 1 low similarity match"), "{form}");
        assert!(form.contains("84.4% alike"), "{form}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn new_uploaders_cant_post_what_the_tagger_blocks(pool: PgPool) {
        moekura_db::settings::set(
            &pool,
            "tagger",
            serde_json::json!({ "new_uploader_blocked": [{ "tag": "ai-generated", "confidence": 50 }] }),
        )
        .await
        .unwrap();
        let mut config = crate::test_support::test_config();
        config.tagger.enabled = true;
        let state = crate::test_support::test_state_with(&pool, config).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(state, routes(max).merge(upload::routes(max)));
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let ai: i32 =
            sqlx::query_scalar("INSERT INTO tags (name) VALUES ('ai-generated') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        async fn stage(app: &TestApp, pool: &PgPool, session: &str, size: u32) -> (String, i64) {
            let png = fixture::png(size, 30);
            let sent = app
                .post_multipart("/uploads", Some(session), &[], Some(("a.png", &png)))
                .await;
            let upload = upload_in(sent.location.as_deref());
            let file = files_of(pool, upload).await[0].id;
            (format!("/uploads/{upload}/assets/{file}"), file)
        }
        let tagged = async |file: i64, confidence: f32| {
            moekura_db::tag_suggestions::save_staged(
                &pool,
                file,
                "test",
                Rating::General,
                0.9,
                &[(ai, confidence)],
            )
            .await
            .unwrap();
        };

        // Not looked at yet, then found AI-generated: refused, saying no
        // more than that.
        let (form, file) = stage(&app, &pool, &alice, 40).await;
        let refused = app
            .post_form(&form, Some(&alice), &[], "rating=g&tags=cat")
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            refused.body.contains("Post failed, try again later."),
            "{}",
            refused.body
        );
        tagged(file, 0.8).await;
        let refused = app
            .post_form(&form, Some(&alice), &[], "rating=g&tags=cat")
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);

        // Not sure enough: posted.
        let (form, file) = stage(&app, &pool, &alice, 44).await;
        tagged(file, 0.3).await;
        let posted = app
            .post_form(&form, Some(&alice), &[], "rating=g&tags=cat")
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);

        // With an active post, nothing's blocked.
        let (form, file) = stage(&app, &pool, &alice, 48).await;
        tagged(file, 0.8).await;
        let posted = app
            .post_form(&form, Some(&alice), &[], "rating=g&tags=cat")
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
    }

    /// A zip of `entries` (`(name, bytes)`).
    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn archives_are_unpacked(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let (a, b) = (fixture::png(40, 30), fixture::png(44, 30));
        let archive = zip_of(&[
            ("set/10.png", &a),
            ("set/2.png", &b),
            ("notes.txt", b"hello"),
            ("__MACOSX/set/._2.png", b"fork"),
        ]);
        let sent = app
            .post_multipart("/uploads", Some(&alice), &[], Some(("art.zip", &archive)))
            .await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let files = files_of(&pool, upload_in(sent.location.as_deref())).await;
        assert_eq!(
            files
                .iter()
                .map(|f| (f.file_name.as_str(), f.status, f.width))
                .collect::<Vec<_>>(),
            [
                ("art.zip/notes.txt", Status::Failed, None),
                ("art.zip/set/2.png", Status::Ready, Some(44)),
                ("art.zip/set/10.png", Status::Ready, Some(40)),
            ]
        );

        // Paths leaving the archive are refused, and nothing is made.
        let evil = zip_of(&[("../evil.png", &a)]);
        let refused = app
            .post_multipart("/uploads", Some(&alice), &[], Some(("evil.zip", &evil)))
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("goes outside it"), "{}", refused.body);

        // Pixiv's animations stay one file.
        let ugoira = zip_of(&[("000000.png", &a), ("000001.png", &a)]);
        let sent = app
            .post_multipart("/uploads", Some(&alice), &[], Some(("u.zip", &ugoira)))
            .await;
        let files = files_of(&pool, upload_in(sent.location.as_deref())).await;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].media_type.as_deref(), Some("ugoira"));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn uploading_a_post_again_leads_to_it(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(40, 30);
        let posted = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &[("rating", "g".to_owned())],
                Some(("a.png", &png)),
            )
            .await;
        let post = post_in(posted.location.as_deref());
        let again = app
            .post_multipart("/uploads", Some(&alice), &[], Some(("a.png", &png)))
            .await;
        let shown = app
            .get(
                &format!("/uploads/{}", upload_in(again.location.as_deref())),
                Some(&alice),
            )
            .await;
        assert_eq!(
            shown.location.as_deref(),
            Some(format!("/posts/{post}").as_str())
        );
        assert!(
            shown.set_cookie.iter().any(|c| c.contains("duplicate")),
            "{:?}",
            shown.set_cookie
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn files_waiting_are_capped(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let sent = app
            .post_multipart(
                "/uploads",
                Some(&alice),
                &[],
                Some(("a.png", &fixture::png(40, 30))),
            )
            .await;
        let upload = upload_in(sent.location.as_deref());
        sqlx::query(
            "INSERT INTO staged_uploads (upload_id, uploader_id, position, status, file_url)
             SELECT upload_id, uploader_id, n, 'pending', 'https://example.com/x.png'
             FROM staged_uploads, generate_series(1, $1 - 2) AS n WHERE upload_id = $2",
        )
        .bind(MAX_WAITING)
        .bind(upload)
        .execute(&pool)
        .await
        .unwrap();
        // One more fits; two don't.
        let two = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[],
                &[
                    ("file", "b.png", &fixture::png(44, 30)),
                    ("file", "c.png", &fixture::png(48, 30)),
                ],
            )
            .await;
        assert_eq!(two.status, StatusCode::TOO_MANY_REQUESTS, "{}", two.body);
        let one = app
            .post_multipart(
                "/uploads",
                Some(&alice),
                &[],
                Some(("b.png", &fixture::png(44, 30))),
            )
            .await;
        assert_eq!(one.status, StatusCode::SEE_OTHER, "{}", one.body);
        let full = app
            .post_multipart(
                "/uploads",
                Some(&alice),
                &[],
                Some(("c.png", &fixture::png(48, 30))),
            )
            .await;
        assert_eq!(full.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            full.body.contains("files waiting to be posted"),
            "{}",
            full.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_list_filters_and_moderators_see_everyones(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec();
        let sent = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[("url", "https://example.com/work/1".to_owned())],
                &[
                    ("file", "a.png", &fixture::png(40, 30)),
                    ("file", "b.gif", &fixture::gif(44, 30)),
                    ("file", "c.svg", &svg),
                ],
            )
            .await;
        let upload = upload_in(sent.location.as_deref());
        let files = files_of(&pool, upload).await;
        let shown = |body: &str| -> Vec<i64> {
            files
                .iter()
                .filter(|f| body.contains(&format!("/uploads/{upload}/assets/{}\"", f.id)))
                .map(|f| f.id)
                .collect()
        };
        let list = async |query: &str, session: &str| {
            app.get(&format!("/uploads?{query}"), Some(session))
                .await
                .body
        };
        let (a, b, c) = (files[0].id, files[1].id, files[2].id);
        assert_eq!(shown(&list("", &alice).await), [a, b, c]);
        assert_eq!(shown(&list("status=failed", &alice).await), [c]);
        assert_eq!(shown(&list("type=gif", &alice).await), [b]);
        assert_eq!(shown(&list("posted=no&type=png", &alice).await), [a]);
        assert_eq!(
            shown(&list("source=https%3A%2F%2Fexample.com%2Fwork", &alice).await),
            [a, b, c]
        );
        assert!(shown(&list("source=https%3A%2F%2Felsewhere", &alice).await).is_empty());
        // Others' files are only for moderators, who can look but not post.
        assert!(shown(&list("user=", &bob).await).is_empty());
        assert_eq!(shown(&list("user=", &moderator).await), [a, b, c]);
        assert_eq!(shown(&list("user=alice", &moderator).await), [a, b, c]);
        assert!(shown(&list("", &moderator).await).is_empty());
        let page = app
            .get(&format!("/uploads/{upload}/assets/{a}"), Some(&moderator))
            .await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("alice's upload"), "{}", page.body);
        assert!(!page.body.contains("name=\"rating\""), "{}", page.body);
        let post = app
            .post_form(
                &format!("/uploads/{upload}/assets/{a}"),
                Some(&moderator),
                &[],
                "rating=g",
            )
            .await;
        assert_eq!(post.status, StatusCode::NOT_FOUND);
        assert_eq!(
            app.get(&format!("/uploads/{upload}/assets/{a}"), Some(&bob))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
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
        // The first is ready when the page is shown; the rest follow.
        assert_ne!(files_of(&pool, upload).await[0].status, Status::Pending);
        let mut files = files_of(&pool, upload).await;
        for _ in 0..100 {
            if files.iter().all(|f| f.status != Status::Pending) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            files = files_of(&pool, upload).await;
        }
        let progress = app
            .get_json(&format!("/uploads/{upload}/status"), Some(&alice))
            .await;
        let progress: serde_json::Value = serde_json::from_str(&progress.body).unwrap();
        assert_eq!(progress["pending"], 0);
        assert_eq!(progress["files"][1]["status"], "ready");
        assert_eq!(
            progress["files"][1]["url"],
            format!("/uploads/{upload}/assets/{}", files[1].id)
        );
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

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn downloads_given_up_on_keep_no_file(pool: PgPool) {
        use moekura_storage::Key;
        use sha2::{Digest, Sha256};

        let mut state = test_state(&pool).await;
        state.fetcher = crate::fetch::Fetcher::new(Duration::from_secs(10), true);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let current = crate::test_support::current_user(&state, &alice).await;
        let uploader_id = current.user.as_ref().unwrap().id;
        let upload = staged_uploads::create_upload(&pool, uploader_id, "", "")
            .await
            .unwrap();
        // The upload's page gives up on its files while the first
        // downloads, as it does on a long upload.
        let png = fixture::png(40, 30);
        let served = {
            let (pool, png) = (pool.clone(), png.clone());
            move || async move {
                sqlx::query("UPDATE staged_uploads SET updated_at = now() - interval '1 hour'")
                    .execute(&pool)
                    .await
                    .unwrap();
                staged_uploads::fail_abandoned(&pool, upload, ABANDONED_AFTER, "gone")
                    .await
                    .unwrap();
                png
            }
        };
        let fetched_late = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let late = {
            let (fetched_late, png) = (fetched_late.clone(), png.clone());
            move || async move {
                fetched_late.store(true, std::sync::atomic::Ordering::SeqCst);
                png
            }
        };
        let origin = Router::new()
            .route("/1.png", get(served))
            .route("/2.png", get(late));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });
        let mut files = Vec::new();
        for position in 0..2 {
            let url = format!("http://{addr}/{}.png", position + 1);
            let slot = Slot {
                upload_id: upload,
                uploader_id,
                position,
                file_name: &url,
                source: &url,
            };
            let file = staged_uploads::create_pending(&pool, slot, &url)
                .await
                .unwrap();
            files.push(file);
        }

        let (first, _) = tokio::sync::oneshot::channel();
        download_pending(
            state.clone(),
            current,
            upload,
            uploader_id,
            None,
            u64::MAX,
            first,
        )
        .await;
        for file in files {
            let file = staged_uploads::by_id(&pool, file).await.unwrap().unwrap();
            assert_eq!((file.status, file.storage_key), (Status::Failed, None));
        }
        let key = Key::original(&hex::encode(Sha256::digest(&png)), "png");
        assert!(!state.storage.exists(&key).await.unwrap());
        // The second wasn't downloaded at all.
        assert!(!fetched_late.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn unread_x_posts_say_why(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let post = "https://x.com/artist/status/1";
        state.sources.remember_unread(post);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let sent = app
            .post_multipart("/uploads", Some(&alice), &[("url", post.to_owned())], None)
            .await;
        assert_eq!(sent.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(sent.body.contains("age-restricted"), "{}", sent.body);
        let uploads: i64 = sqlx::query_scalar("SELECT count(*) FROM uploads")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(uploads, 0);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn only_web_pages_are_sources(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let script = "javascript:alert(1)";
        let sent = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[("url", script.to_owned())],
                &[
                    ("file", "a.png", &fixture::png(40, 30)),
                    ("file", "b.png", &fixture::png(44, 30)),
                ],
            )
            .await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let upload = upload_in(sent.location.as_deref());
        let saved = staged_uploads::upload_by_id(&pool, upload)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.source, "");
        assert!(
            files_of(&pool, upload)
                .await
                .iter()
                .all(|f| f.source.is_empty())
        );

        // One kept from before is shown, but not as a link.
        let old = staged_uploads::create_upload(&pool, saved.uploader_id, script, "")
            .await
            .unwrap();
        let mut ids = Vec::new();
        for position in 0..2 {
            let slot = Slot {
                upload_id: old,
                uploader_id: saved.uploader_id,
                position,
                file_name: "x.svg",
                source: "",
            };
            ids.push(
                staged_uploads::create_failed(&pool, slot, "Not an image.", None)
                    .await
                    .unwrap(),
            );
        }
        for path in [
            format!("/uploads/{old}"),
            format!("/uploads/{old}/assets/{}", ids[0]),
        ] {
            let page = app.get(&path, Some(&alice)).await;
            assert_eq!(page.status, StatusCode::OK, "{path}");
            assert!(page.body.contains(script), "{}", page.body);
            assert!(!page.body.contains("href=\"javascript"), "{}", page.body);
        }

        // Nor are a source's page and profiles that aren't web pages.
        let work = "https://example.com/work/1";
        state.sources.remember(
            work,
            SourceInfo {
                site: "Example",
                page_url: script.to_owned(),
                artist_name: Some("someone".to_owned()),
                profile_urls: vec![script.to_owned(), "https://example.com/someone".to_owned()],
                ..SourceInfo::default()
            },
        );
        let query: String = url::form_urlencoded::byte_serialize(work.as_bytes()).collect();
        let panel = app
            .get(&format!("/uploads/source-data?url={query}"), Some(&alice))
            .await
            .body;
        assert!(panel.contains(">Example</dd>"), "{panel}");
        assert!(
            panel.contains("someone\" rel=\"noopener noreferrer nofollow\""),
            "{panel}"
        );
        assert!(!panel.contains("href=\"javascript"), "{panel}");
        assert!(!panel.contains(">javascript:alert(1)</a>"), "{panel}");
    }

    /// State whose sources and downloads reach `127.0.0.1`.
    async fn local_state(pool: &PgPool) -> AppState {
        let mut state = test_state(pool).await;
        state.fetcher = crate::fetch::Fetcher::new(Duration::from_secs(10), true);
        state.sources = Arc::new(crate::sources::Sources::new(
            true,
            moekura_core::config::SourcesConfig::default(),
        ));
        state
    }

    /// Held by tests that keep download turns taken a while: two at once
    /// could each wait for turns the other holds.
    static HOLDING_TURNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Waits until upload `upload`'s downloads are done; its files then.
    async fn downloaded(pool: &PgPool, upload: i64) -> Vec<Staged> {
        for _ in 0..200 {
            let files = files_of(pool, upload).await;
            if files.iter().all(|f| f.status != Status::Pending) {
                return files;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("upload {upload} is still downloading");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn a_works_many_files_are_taken_once_and_only_when_asked(pool: PgPool) {
        let png = fixture::png(40, 30);
        let origin = Router::new().route("/1.png", get(move || async move { png }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let state = local_state(&pool).await;
        let file = |n: usize| format!("http://{addr}/{n}.png");
        let work = |name: &str, files: Vec<String>| {
            let page = format!("http://{addr}/{name}");
            state.sources.remember(
                &page,
                SourceInfo {
                    site: "Example",
                    page_url: page.clone(),
                    files,
                    ..SourceInfo::default()
                },
            );
            page
        };
        let twice = work("twice", vec![file(1), file(1), file(2), file(1)]);
        let enough = work("enough", (1..=MAX_FILES).map(file).collect());
        // More than are taken at once, and some twice.
        let many_files: Vec<String> = (1..=25).chain(1..=5).map(file).collect();
        let many = work("many", many_files);
        let max = 10 * 1024 * 1024;
        let app = TestApp::new(state.clone(), routes(max).merge(upload::routes(max)));
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let send = async |link: &str, all: bool| {
            let mut fields = vec![("url", link.to_owned())];
            if all {
                fields.push(("all_files", "1".to_owned()));
            }
            app.post_multipart("/uploads", Some(&alice), &fields, None)
                .await
        };

        // A file listed twice is downloaded once.
        let sent = send(&twice, false).await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let files = downloaded(&pool, upload_in(sent.location.as_deref())).await;
        assert_eq!(
            files
                .iter()
                .map(|f| f.file_url.clone().unwrap())
                .collect::<Vec<_>>(),
            [file(1), file(2)]
        );

        // As many as files sent at once are taken without asking.
        let sent = send(&enough, false).await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let files = downloaded(&pool, upload_in(sent.location.as_deref())).await;
        assert_eq!(files.len(), MAX_FILES);

        // More only when the box is ticked.
        let uploads = async || {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM uploads")
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        let before = uploads().await;
        let asked = send(&many, false).await;
        assert_eq!(asked.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            asked.body.contains("This work has 25 files."),
            "{}",
            asked.body
        );
        assert!(asked.body.contains("name=\"all_files\""), "{}", asked.body);
        assert!(asked.body.contains("data-upload-confirm"), "{}", asked.body);
        assert_eq!(uploads().await, before);
        let sent = send(&many, true).await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let files = downloaded(&pool, upload_in(sent.location.as_deref())).await;
        assert_eq!(files.len(), 25);
        assert_eq!(files[0].status, Status::Ready);
        assert!(files[1..].iter().all(|f| f.status == Status::Failed));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn downloads_skip_files_given_up_and_stop_at_the_budget(pool: PgPool) {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let asked_first = Arc::new(AtomicUsize::new(0));
        let (one, two) = (fixture::png(40, 30), fixture::png(44, 30));
        let budget = two.len() as u64;
        let counter = asked_first.clone();
        let origin = Router::new()
            .route(
                "/1.png",
                get(move || async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                    one
                }),
            )
            .route("/2.png", get(move || async move { two }))
            .route("/3.png", get(|| async { fixture::png(48, 30) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let state = local_state(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let current = current_user(&state, &alice).await;
        let alice = current.user.as_ref().unwrap().id;
        let work = format!("http://{addr}/work");
        let upload = staged_uploads::create_upload(&pool, alice, &work, "")
            .await
            .unwrap();
        let mut ids = Vec::new();
        for n in 1..=3 {
            let file = format!("http://{addr}/{n}.png");
            let slot = Slot {
                upload_id: upload,
                uploader_id: alice,
                position: n,
                file_name: &file,
                source: &work,
            };
            ids.push(
                staged_uploads::create_pending(&pool, slot, &file)
                    .await
                    .unwrap(),
            );
        }
        // The first was given up on (as abandoned) before its turn came.
        staged_uploads::failed(&pool, ids[0], "Given up.", None)
            .await
            .unwrap();

        let (first, _) = tokio::sync::oneshot::channel();
        download_pending(state.clone(), current, upload, alice, None, budget, first).await;
        let files = files_of(&pool, upload).await;
        assert_eq!(asked_first.load(Ordering::Relaxed), 0);
        assert_eq!(
            files
                .iter()
                .map(|f| (f.status, f.width))
                .collect::<Vec<_>>(),
            [
                (Status::Failed, None),
                (Status::Ready, Some(44)),
                (Status::Failed, None)
            ]
        );
        assert_eq!(files[0].error.as_deref(), Some("Given up."));
        assert!(
            files[2]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("it may download")),
            "{:?}",
            files[2].error
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn downloads_wait_for_a_turn_without_being_given_up(pool: PgPool) {
        let origin = Router::new().route("/1.png", get(|| async { fixture::png(40, 30) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let state = local_state(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let current = current_user(&state, &alice).await;
        let alice = current.user.as_ref().unwrap().id;
        let (work, file) = (
            format!("http://{addr}/work"),
            format!("http://{addr}/1.png"),
        );
        let upload = staged_uploads::create_upload(&pool, alice, &work, "")
            .await
            .unwrap();
        let slot = Slot {
            upload_id: upload,
            uploader_id: alice,
            position: 0,
            file_name: &file,
            source: &work,
        };
        staged_uploads::create_pending(&pool, slot, &file)
            .await
            .unwrap();
        // Untouched long enough to look abandoned.
        sqlx::query("UPDATE staged_uploads SET updated_at = now() - interval '1 hour'")
            .execute(&pool)
            .await
            .unwrap();

        // Every turn is taken, by other uploads' downloads say.
        let _alone = HOLDING_TURNS.lock().await;
        let taken = DOWNLOADS
            .acquire_many(u32::try_from(MAX_DOWNLOADS).unwrap())
            .await
            .unwrap();
        let (first, _) = tokio::sync::oneshot::channel();
        let downloading = tokio::spawn(download_pending(
            state.clone(),
            current.clone(),
            upload,
            alice,
            None,
            u64::MAX,
            first,
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(files_of(&pool, upload).await[0].status, Status::Pending);
        // Still wanted while it waits, so a look at the upload doesn't give
        // up on it.
        let given_up = staged_uploads::fail_abandoned(&pool, upload, ABANDONED_AFTER, "gone")
            .await
            .unwrap();
        assert_eq!(given_up, 0);

        drop(taken);
        downloading.await.unwrap();
        assert_eq!(files_of(&pool, upload).await[0].status, Status::Ready);
    }

    /// Makes an upload by `uploader` of a pending file for each of `files`;
    /// returns its id.
    async fn pending_upload(pool: &PgPool, uploader: i64, files: &[String]) -> i64 {
        let upload = staged_uploads::create_upload(pool, uploader, "", "")
            .await
            .unwrap();
        for (position, file) in (0..).zip(files) {
            let slot = Slot {
                upload_id: upload,
                uploader_id: uploader,
                position,
                file_name: file,
                source: "",
            };
            staged_uploads::create_pending(pool, slot, file)
                .await
                .unwrap();
        }
        upload
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn downloads_that_fail_count_toward_the_budget(pool: PgPool) {
        // A file that goes on past the size limit, without saying its
        // length first.
        let endless = || async {
            let chunk = axum::body::Bytes::from(vec![0; 64 * 1024]);
            let chunks = (0..64).map(move |_| Ok::<_, std::io::Error>(chunk.clone()));
            axum::body::Body::from_stream(futures_util::stream::iter(chunks))
        };
        let origin = Router::new()
            .route("/endless", get(endless))
            .route("/1.png", get(|| async { fixture::png(40, 30) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let mut config = crate::test_support::test_config();
        config.media.max_upload_mb = 1;
        let mut state = crate::test_support::test_state_with(&pool, config).await;
        state.fetcher = crate::fetch::Fetcher::new(Duration::from_secs(10), true);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let current = current_user(&state, &alice).await;
        let alice = current.user.as_ref().unwrap().id;
        let files = [
            format!("http://{addr}/endless"),
            format!("http://{addr}/1.png"),
        ];
        let upload = pending_upload(&pool, alice, &files).await;

        // The first fails once past the limit, having taken up the budget
        // all the same.
        let (first, _) = tokio::sync::oneshot::channel();
        download_pending(
            state.clone(),
            current,
            upload,
            alice,
            None,
            512 * 1024,
            first,
        )
        .await;
        let files = files_of(&pool, upload).await;
        assert_eq!(
            files.iter().map(|f| f.status).collect::<Vec<_>>(),
            [Status::Failed, Status::Failed]
        );
        assert!(
            files[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("larger than 1 MB")),
            "{:?}",
            files[0].error
        );
        assert!(
            files[1]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("it may download")),
            "{:?}",
            files[1].error
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn one_users_downloads_leave_turns_for_others(pool: PgPool) {
        use std::sync::atomic::AtomicUsize;

        // Answers once let through, counting who asked meanwhile.
        let (gate, asked) = (Arc::new(Semaphore::new(0)), Arc::new(AtomicUsize::new(0)));
        let slow = {
            let (gate, asked) = (gate.clone(), asked.clone());
            move || async move {
                asked.fetch_add(1, Ordering::Relaxed);
                let _through = gate.acquire().await.unwrap();
                fixture::png(40, 30)
            }
        };
        let origin = Router::new()
            .route("/slow.png", get(slow))
            .route("/1.png", get(|| async { fixture::png(44, 30) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });

        let _alone = HOLDING_TURNS.lock().await;
        let state = local_state(&pool).await;
        // Turns are kept by user id for the whole process: these are ids
        // no other test's users have.
        sqlx::query("SELECT setval(pg_get_serial_sequence('users', 'id'), 1000000)")
            .execute(&pool)
            .await
            .unwrap();
        let user = async |name: &str| {
            let session = session_for(&pool, name, SystemRole::Member).await;
            let current = current_user(&state, &session).await;
            let id = current.user.as_ref().unwrap().id;
            (current, id)
        };
        let ((alice_current, alice), (bob_current, bob)) = (user("alice").await, user("bob").await);
        // Alice sends more slow links than the whole site has turns.
        let mut alices = Vec::new();
        let mut downloads = Vec::new();
        for _ in 0..=MAX_DOWNLOADS {
            let upload = pending_upload(&pool, alice, &[format!("http://{addr}/slow.png")]).await;
            let (first, _) = tokio::sync::oneshot::channel();
            let downloading = download_pending(
                state.clone(),
                alice_current.clone(),
                upload,
                alice,
                None,
                u64::MAX,
                first,
            );
            downloads.push(tokio::spawn(downloading));
            alices.push(upload);
        }
        for _ in 0..200 {
            if asked.load(Ordering::Relaxed) == MAX_USER_DOWNLOADS {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Bob's is downloaded meanwhile, while the rest of Alice's wait for
        // her own turns.
        let bobs = pending_upload(&pool, bob, &[format!("http://{addr}/1.png")]).await;
        let (first, _) = tokio::sync::oneshot::channel();
        tokio::time::timeout(
            Duration::from_secs(5),
            download_pending(state.clone(), bob_current, bobs, bob, None, u64::MAX, first),
        )
        .await
        .expect("Bob's download waited for Alice's");
        assert_eq!(files_of(&pool, bobs).await[0].status, Status::Ready);
        assert_eq!(asked.load(Ordering::Relaxed), MAX_USER_DOWNLOADS);

        gate.add_permits(MAX_DOWNLOADS + 1);
        for downloading in downloads {
            downloading.await.unwrap();
        }
        assert_eq!(asked.load(Ordering::Relaxed), MAX_DOWNLOADS + 1);
        for upload in alices {
            assert_eq!(files_of(&pool, upload).await[0].status, Status::Ready);
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn long_form_fields_are_refused(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let routes = routes(max)
            .merge(upload::routes(max))
            .merge(crate::image_search::routes(max))
            .merge(crate::danbooru::routes(max));
        let app = TestApp::new(state, routes);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let long = format!("https://example.com/{}", "a".repeat(upload::LINK_FIELD_MAX));
        for (path, field, value) in [
            ("/uploads", "url", long.clone()),
            ("/uploads", "ref", long.clone()),
            ("/upload", "source", long.clone()),
            ("/upload", "tags", "a ".repeat(upload::TEXT_FIELD_MAX)),
            ("/iqdb_queries", "url", long.clone()),
        ] {
            let sent = app
                .post_multipart(path, Some(&alice), &[(field, value)], None)
                .await;
            assert_eq!(
                sent.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{path} {field}"
            );
            assert!(
                sent.body.contains("A form field is too long."),
                "{path} {field}: {}",
                sent.body
            );
        }
        // Danbooru's image search takes its fields without a file in a
        // plain form too.
        let form = format!("url={}", "a".repeat(100 * 1024));
        let sent = app
            .post_form("/iqdb_queries.json", Some(&alice), &[], &form)
            .await;
        assert_eq!(sent.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            sent.body.contains("A form field is too long."),
            "{}",
            sent.body
        );
        // A short one is read as before.
        let sent = app
            .post_form("/iqdb_queries.json", Some(&alice), &[], "url=+")
            .await;
        assert!(sent.body.contains("Give `search[url]`"), "{}", sent.body);
    }

    #[test]
    fn sweeps_only_stale_scratch_files() {
        let dir = std::env::temp_dir().join(format!(
            "moekura-sweep-{}-{}",
            std::process::id(),
            crate::shared::tests::fresh()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::time::SystemTime::now() - 2 * STALE_AFTER;
        let make = |name: &str, age: Option<std::time::SystemTime>| {
            let path = dir.join(name);
            if name.ends_with('/') {
                std::fs::create_dir_all(path.join("inner")).unwrap();
                std::fs::write(path.join("inner/archive-000"), b"x").unwrap();
            } else {
                std::fs::write(&path, b"x").unwrap();
            }
            if let Some(age) = age {
                std::fs::File::open(&path)
                    .unwrap()
                    .set_modified(age)
                    .unwrap();
            }
        };
        make("upload-old", Some(old));
        make("search-old/", Some(old));
        make("unpacked-old/", Some(old));
        make("avatar-old.webp", Some(old));
        make("upload-new", None);
        make("unpacked-new/", None);
        // Others' (a worker's, say) are left alone, however old.
        make("job-media-1-2/", Some(old));
        make("stored-1", Some(old));
        assert_eq!(sweep(&dir, STALE_AFTER), 4);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["job-media-1-2", "stored-1", "unpacked-new", "upload-new"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn unpacking_cleans_up_after_a_dropped_request(pool: PgPool) {
        let state = test_state(&pool).await;
        // Large enough that unpacking it takes a while.
        let zeros = vec![0; 32 * 1024 * 1024];
        let archive = zip_of(&[("a.bin", &zeros), ("b.bin", &zeros)]);
        let mut writer = upload::TempWriter::create(&state.work_dir).await.unwrap();
        writer.write(&archive).await.unwrap();
        let file = writer.finish().await.unwrap();
        let unpacking_dirs = || {
            std::fs::read_dir(&state.work_dir)
                .unwrap()
                .filter(|e| {
                    e.as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("unpacked-")
                })
                .count()
        };
        // The request ends (the connection closed) while it's unpacking.
        tokio::select! {
            _ = unpack_archives(&state, vec![file]) => panic!("unpacked before it was dropped"),
            () = async {
                while unpacking_dirs() == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            } => {}
        }
        for _ in 0..200 {
            if unpacking_dirs() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the unpacked files were left behind");
    }
}
