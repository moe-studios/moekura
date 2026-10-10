//! Files sent in pieces, with the [tus](https://tus.io) protocol (1.0.0,
//! with its creation and termination extensions). A proxy or CDN in front
//! of the site may limit how large one request can be (Cloudflare's limit
//! is 100 MB); sent in pieces, each in its own request, a file can be
//! larger, and a piece that fails is sent again without starting over.
//!
//! `POST /uploads/files` begins a transfer of a file of `Upload-Length`
//! bytes, named by `Upload-Metadata`'s `filename` and sent for its
//! `purpose` (`upload`, the default; `replace`, to replace a post's file;
//! or `search`, to search by image), and answers with the transfer's URL. A `PATCH` there sends the next piece, from
//! `Upload-Offset`; `HEAD` says how much has come, and `DELETE` gives up.
//! The pieces are kept in storage, so that any of several web servers can
//! take each one. Once the whole file has come, a form for its purpose
//! names it by its token in a `transfer` field instead of sending it, and
//! [`take`] puts it together. Transfers left idle are removed ([`prune`]).

use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::StreamExt;
use moekura_core::permissions::Permission;
use moekura_core::posts::SOURCE_MAX_LEN;
use moekura_db::transfers::{self, Purpose, Transfer};
use moekura_storage::Key;
use tokio::io::AsyncWriteExt;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::upload::{self, TempUpload, TempWriter, UploadError};

/// Where transfers are begun; each one is at `/uploads/files/<token>`.
pub const FILES: &str = "/uploads/files";
/// The version of the protocol spoken.
const TUS_VERSION: &str = "1.0.0";
/// The type of a `PATCH`'s body.
const OFFSET_TYPE: &str = "application/offset+octet-stream";
/// A transfer no piece came for in this long is removed.
const IDLE_AFTER: Duration = Duration::from_secs(60 * 60);
/// Idle transfers removed at once (see [`prune`]).
const PRUNE_BATCH: i64 = 100;

const TUS_RESUMABLE: HeaderName = HeaderName::from_static("tus-resumable");
const TUS_VERSION_HEADER: HeaderName = HeaderName::from_static("tus-version");
const TUS_EXTENSION: HeaderName = HeaderName::from_static("tus-extension");
const TUS_MAX_SIZE: HeaderName = HeaderName::from_static("tus-max-size");
const UPLOAD_LENGTH: HeaderName = HeaderName::from_static("upload-length");
const UPLOAD_OFFSET: HeaderName = HeaderName::from_static("upload-offset");
const UPLOAD_METADATA: HeaderName = HeaderName::from_static("upload-metadata");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(FILES, post(begin).options(options))
        .route(
            "/uploads/files/{token}",
            axum::routing::head(offset).patch(append).delete(cancel),
        )
}

/// A response saying the protocol's version, as each must.
fn tus(status: StatusCode) -> Response {
    let mut response = status.into_response();
    response
        .headers_mut()
        .insert(TUS_RESUMABLE, HeaderValue::from_static(TUS_VERSION));
    response
}

/// A refusal, boxed to keep results small.
struct Refused(Box<Response>);

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        *self.0
    }
}

impl From<Response> for Refused {
    fn from(response: Response) -> Self {
        Self(Box::new(response))
    }
}

/// A response saying why in plain text.
fn says(status: StatusCode, message: impl Into<String>) -> Response {
    let mut response = (status, message.into()).into_response();
    response
        .headers_mut()
        .insert(TUS_RESUMABLE, HeaderValue::from_static(TUS_VERSION));
    response
}

/// A refusal, saying why in plain text.
fn refused(status: StatusCode, message: impl Into<String>) -> Refused {
    says(status, message).into()
}

/// The refusal for an upload error, its message logged when it's ours.
fn upload_refused(error: &UploadError) -> Refused {
    if let UploadError::Internal(detail) = error {
        tracing::error!(error = %detail, "file transfer failed");
        return refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong on our side. Please try again.",
        );
    }
    let mut response = says(upload::error_status(error), error.to_string());
    if let UploadError::TooFast(secs) = error {
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from(*secs));
    }
    response.into()
}

fn internal(error: impl std::fmt::Display) -> Refused {
    upload_refused(&UploadError::Internal(error.to_string()))
}

fn header_value(value: impl ToString) -> HeaderValue {
    HeaderValue::from_str(&value.to_string()).expect("numbers and paths are valid header values")
}

/// What a user must be allowed to do to send a file for `purpose`.
fn permission(purpose: Purpose) -> Permission {
    match purpose {
        Purpose::Upload => Permission::Upload,
        Purpose::Replace => Permission::ReplacePosts,
        Purpose::Search => Permission::ViewPosts,
    }
}

/// The user sending: logged in (files wait in their name), and allowed to
/// do what the file is for. Only what a transfer is begun for is known;
/// once it's begun, its sender may go on.
fn sender(current: &CurrentUser, purpose: Option<Purpose>) -> Result<i64, Refused> {
    let allowed = purpose.is_none_or(|purpose| current.can(permission(purpose)));
    match (&current.user, allowed) {
        (Some(user), true) => Ok(user.id),
        (Some(_), false) => Err(refused(StatusCode::FORBIDDEN, "You can't do that.")),
        (None, _) => Err(refused(StatusCode::UNAUTHORIZED, "Log in to send files.")),
    }
}

/// Refuses a request that doesn't speak this version of the protocol.
fn check_version(headers: &HeaderMap) -> Result<(), Refused> {
    if headers
        .get(&TUS_RESUMABLE)
        .is_some_and(|v| v == TUS_VERSION)
    {
        return Ok(());
    }
    let mut response = says(
        StatusCode::PRECONDITION_FAILED,
        format!("Send Tus-Resumable: {TUS_VERSION}."),
    );
    response
        .headers_mut()
        .insert(TUS_VERSION_HEADER, HeaderValue::from_static(TUS_VERSION));
    Err(response.into())
}

/// A header holding a number of bytes, if it does.
fn bytes_header(headers: &HeaderMap, name: &HeaderName) -> Option<u64> {
    let value = headers.get(name)?.to_str().ok()?;
    // Digits only: `parse` would take a sign too.
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// The value an `Upload-Metadata` header gives `wanted`, if any: its
/// pairs are a key and a value in base64, separated by commas.
fn metadata_value(metadata: &str, wanted: &str) -> Option<String> {
    metadata.split(',').find_map(|pair| {
        let (key, value) = pair.trim().split_once(' ')?;
        if key != wanted {
            return None;
        }
        let bytes = STANDARD.decode(value.trim()).ok()?;
        let value = String::from_utf8_lossy(&bytes);
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// The `filename` an `Upload-Metadata` header gives, if any.
fn file_name(metadata: &str) -> Option<String> {
    metadata_value(metadata, "filename").map(|name| name.chars().take(SOURCE_MAX_LEN).collect())
}

fn max_bytes(state: &AppState) -> u64 {
    state.media.config().max_upload_mb * 1024 * 1024
}

/// Says what this server takes: the protocol's version, its extensions,
/// and the largest file.
async fn options(State(state): State<AppState>) -> Response {
    let mut response = tus(StatusCode::NO_CONTENT);
    let headers = response.headers_mut();
    headers.insert(TUS_VERSION_HEADER, HeaderValue::from_static(TUS_VERSION));
    headers.insert(
        TUS_EXTENSION,
        HeaderValue::from_static("creation,termination"),
    );
    headers.insert(TUS_MAX_SIZE, header_value(max_bytes(&state)));
    response
}

/// Begins a transfer, if the sender may upload the file now.
async fn begin(
    State(state): State<AppState>,
    current: CurrentUser,
    headers: HeaderMap,
) -> Result<Response, Refused> {
    let metadata = headers
        .get(&UPLOAD_METADATA)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let purpose = match metadata_value(metadata, "purpose") {
        None => Purpose::Upload,
        Some(purpose) => purpose.parse().map_err(|()| {
            refused(
                StatusCode::BAD_REQUEST,
                "The purpose may be upload, replace or search.",
            )
        })?,
    };
    let uploader_id = sender(&current, Some(purpose))?;
    check_version(&headers)?;
    let Some(length) = bytes_header(&headers, &UPLOAD_LENGTH) else {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "Send the file's size as Upload-Length.",
        ));
    };
    let max_mb = state.media.config().max_upload_mb;
    if length > max_bytes(&state) {
        return Err(refused(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("The file is larger than {max_mb} MB."),
        ));
    }
    if length == 0 {
        return Err(refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The file is empty.",
        ));
    }
    let name = file_name(metadata).unwrap_or_else(|| "file".to_owned());
    if purpose == Purpose::Upload {
        // What would refuse the upload later refuses the file now, before
        // it's sent for nothing. Pace is counted once, by the form.
        let allowance = upload::allowance(&state, &current)
            .await
            .map_err(internal)?;
        if let Some(refusal) = allowance.refusal {
            return Err(upload_refused(&UploadError::Limit(refusal)));
        }
        let user = current.user.as_ref().expect("a sender is logged in");
        crate::uploads::room(&state, user)
            .await
            .map_err(|e| upload_refused(&e))?;
    }
    let db = state.db.primary();
    let sending = transfers::unfinished(db, uploader_id)
        .await
        .map_err(internal)?;
    if sending >= transfers::MAX_PER_USER {
        return Err(upload_refused(&UploadError::Limit(format!(
            "You're sending {sending} files already, the most at once. \
             Wait for them to finish, or for those you gave up on to expire (in an hour)."
        ))));
    }
    let length = i64::try_from(length).map_err(internal)?;
    let (token, _) = transfers::create(db, uploader_id, &name, purpose, length)
        .await
        .map_err(internal)?;
    let mut response = tus(StatusCode::CREATED);
    response
        .headers_mut()
        .insert(LOCATION, header_value(format!("{FILES}/{token}")));
    Ok(response)
}

/// The sender's transfer named `token`.
async fn find(state: &AppState, uploader_id: i64, token: &str) -> Result<Transfer, Refused> {
    transfers::find(state.db.primary(), uploader_id, token)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            refused(
                StatusCode::NOT_FOUND,
                "This file transfer doesn't exist, or expired.",
            )
        })
}

/// How much of the file has come.
async fn offset(
    State(state): State<AppState>,
    current: CurrentUser,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Response, Refused> {
    let uploader_id = sender(&current, None)?;
    check_version(&headers)?;
    let transfer = find(&state, uploader_id, &token).await?;
    let mut response = tus(StatusCode::OK);
    let headers = response.headers_mut();
    headers.insert(UPLOAD_OFFSET, header_value(transfer.received));
    headers.insert(UPLOAD_LENGTH, header_value(transfer.length));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// A scratch file for a piece on its way to storage, removed when
/// dropped: when the request ends early too.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Takes the next piece of the file, from `Upload-Offset`.
async fn append(
    State(state): State<AppState>,
    current: CurrentUser,
    headers: HeaderMap,
    Path(token): Path<String>,
    body: Body,
) -> Result<Response, Refused> {
    let uploader_id = sender(&current, None)?;
    check_version(&headers)?;
    if headers.get(CONTENT_TYPE).is_none_or(|v| v != OFFSET_TYPE) {
        return Err(refused(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("Send pieces as {OFFSET_TYPE}."),
        ));
    }
    let Some(offset) = bytes_header(&headers, &UPLOAD_OFFSET) else {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "Say where the piece goes with Upload-Offset.",
        ));
    };
    let transfer = find(&state, uploader_id, &token).await?;
    let conflict = |received: i64| {
        let mut response = says(
            StatusCode::CONFLICT,
            format!("The file has {received} bytes so far; send the piece from there."),
        );
        response
            .headers_mut()
            .insert(UPLOAD_OFFSET, header_value(received));
        Refused::from(response)
    };
    let offset = i64::try_from(offset).map_err(|_| conflict(transfer.received))?;
    if offset != transfer.received {
        return Err(conflict(transfer.received));
    }

    // The piece goes to a scratch file first: storage wants its size.
    let random = hex::encode(moekura_core::tokens::NewToken::generate().hash);
    let scratch = Scratch(
        state
            .work_dir
            .join(format!("upload-piece-{}", &random[..24])),
    );
    let mut file = tokio::fs::File::create(&scratch.0)
        .await
        .map_err(internal)?;
    let room = u64::try_from(transfer.length - transfer.received).unwrap_or(0);
    let mut size = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            refused(
                StatusCode::BAD_REQUEST,
                "The piece was interrupted. Send it again.",
            )
        })?;
        size += chunk.len() as u64;
        if size > room {
            return Err(refused(
                StatusCode::PAYLOAD_TOO_LARGE,
                "The piece goes past the end of the file.",
            ));
        }
        file.write_all(&chunk).await.map_err(internal)?;
    }
    file.flush().await.map_err(internal)?;
    drop(file);
    let received = if size == 0 {
        transfer.received
    } else {
        let size = i64::try_from(size).map_err(internal)?;
        let mut tx = state.db.primary().begin().await.map_err(internal)?;
        // Another request may have sent this piece meanwhile, or the file
        // been used or given up on.
        let Some(locked) = transfers::lock(&mut tx, transfer.id)
            .await
            .map_err(internal)?
        else {
            return Err(refused(
                StatusCode::NOT_FOUND,
                "This file transfer doesn't exist, or expired.",
            ));
        };
        if locked.received != offset {
            return Err(conflict(locked.received));
        }
        let index = u32::try_from(locked.parts).map_err(internal)?;
        state
            .storage
            .put_file(&part_key(&locked, index), &scratch.0)
            .await
            .map_err(internal)?;
        transfers::add_part(&mut tx, locked.id, size)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        offset + size
    };
    let mut response = tus(StatusCode::NO_CONTENT);
    response
        .headers_mut()
        .insert(UPLOAD_OFFSET, header_value(received));
    Ok(response)
}

/// Gives up on the transfer, removing what was sent.
async fn cancel(
    State(state): State<AppState>,
    current: CurrentUser,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Response, Refused> {
    let uploader_id = sender(&current, None)?;
    check_version(&headers)?;
    let transfer = transfers::take(state.db.primary(), uploader_id, &token)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            refused(
                StatusCode::NOT_FOUND,
                "This file transfer doesn't exist, or expired.",
            )
        })?;
    remove_parts(&state, &transfer).await;
    Ok(tus(StatusCode::NO_CONTENT))
}

/// Where piece `index` of `transfer` is stored.
fn part_key(transfer: &Transfer, index: u32) -> Key {
    Key::transfer_part(&hex::encode(&transfer.token_hash), index)
}

/// Removes `transfer`'s pieces from storage, and the one after them: a
/// piece stored whose recording failed.
async fn remove_parts(state: &AppState, transfer: &Transfer) {
    let parts = u32::try_from(transfer.parts).unwrap_or(0);
    for index in 0..=parts {
        if let Err(error) = state.storage.delete(&part_key(transfer, index)).await {
            tracing::warn!(transfer = transfer.id, index, %error, "file transfer's piece not removed");
        }
    }
}

/// The whole file of `uploader_id`'s transfer named `token`, put together
/// for a form for `purpose` that named it. The transfer is used up,
/// whatever happens.
pub(crate) async fn take(
    state: &AppState,
    uploader_id: i64,
    token: &str,
    purpose: Purpose,
) -> Result<TempUpload, UploadError> {
    let token = token.trim();
    let transfer = transfers::take_for(state.db.primary(), uploader_id, token, purpose)
        .await?
        .ok_or_else(|| {
            UploadError::Invalid(
                "A file sent earlier wasn't found: it may have waited for over an hour. \
                 Please send it again."
                    .into(),
            )
        })?;
    let file = assemble(state, &transfer).await;
    remove_parts(state, &transfer).await;
    file
}

/// Puts `transfer`'s pieces together into one file.
async fn assemble(state: &AppState, transfer: &Transfer) -> Result<TempUpload, UploadError> {
    if !transfer.is_complete() {
        return Err(UploadError::Invalid(format!(
            "{} wasn't sent completely. Please send it again.",
            transfer.file_name
        )));
    }
    let length = u64::try_from(transfer.length).unwrap_or(u64::MAX);
    if length > max_bytes(state) {
        return Err(UploadError::Invalid(format!(
            "The file is larger than {} MB.",
            state.media.config().max_upload_mb
        )));
    }
    let failed = |e: moekura_storage::StorageError| {
        UploadError::Internal(format!("reading a file transfer's piece: {e}"))
    };
    let mut writer = TempWriter::create(&state.work_dir).await?;
    for index in 0..u32::try_from(transfer.parts).unwrap_or(0) {
        let mut part = state
            .storage
            .read(&part_key(transfer, index), None)
            .await
            .map_err(failed)?
            .stream;
        while let Some(chunk) = part.next().await {
            writer.write(&chunk.map_err(failed)?).await?;
        }
    }
    if writer.written() != length {
        return Err(UploadError::Internal(format!(
            "file transfer {}'s pieces came to {} bytes, not {length}",
            transfer.id,
            writer.written()
        )));
    }
    let mut file = writer.finish().await?;
    file.set_name(&transfer.file_name);
    Ok(file)
}

/// Removes transfers left idle for [`IDLE_AFTER`], and their pieces. Runs
/// every hour.
pub async fn prune(state: &AppState) {
    let mut removed = 0;
    loop {
        let idle = match transfers::take_idle(state.db.primary(), IDLE_AFTER, PRUNE_BATCH).await {
            Ok(idle) => idle,
            Err(error) => {
                tracing::warn!(%error, "could not remove idle file transfers");
                break;
            }
        };
        for transfer in &idle {
            remove_parts(state, transfer).await;
        }
        removed += idle.len();
        if (idle.len() as i64) < PRUNE_BATCH {
            break;
        }
    }
    if removed > 0 {
        tracing::info!(removed, "removed idle file transfers");
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use moekura_db::staged_uploads;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, TestResponse, fixture, session_for, test_state};

    const TUS: (&str, &str) = ("tus-resumable", TUS_VERSION);

    async fn app(pool: &PgPool) -> (TestApp, AppState) {
        let state = test_state(pool).await;
        let max = max_bytes(&state);
        let routes = routes()
            .merge(crate::uploads::routes(max))
            .merge(crate::upload::routes(max))
            .merge(crate::replacements::routes(max))
            .merge(crate::image_search::routes(max));
        (TestApp::new(state.clone(), routes), state)
    }

    /// Begins a transfer of `length` bytes named `name`, returning its URL.
    async fn begin(app: &TestApp, session: &str, length: usize, name: &str) -> String {
        begin_for(app, session, length, name, "upload").await
    }

    /// [`begin`], for `purpose`.
    async fn begin_for(
        app: &TestApp,
        session: &str,
        length: usize,
        name: &str,
        purpose: &str,
    ) -> String {
        let metadata = format!(
            "filename {},purpose {}",
            STANDARD.encode(name),
            STANDARD.encode(purpose)
        );
        let length = length.to_string();
        let created = app
            .request(
                "POST",
                FILES,
                Some(session),
                &[
                    TUS,
                    ("upload-length", &length),
                    ("upload-metadata", &metadata),
                ],
                Vec::new(),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        assert_eq!(created.headers["tus-resumable"], TUS_VERSION);
        created.location.expect("the transfer's URL")
    }

    async fn piece(
        app: &TestApp,
        session: &str,
        url: &str,
        offset: usize,
        bytes: &[u8],
    ) -> TestResponse {
        let offset = offset.to_string();
        app.request(
            "PATCH",
            url,
            Some(session),
            &[
                TUS,
                ("upload-offset", &offset),
                ("content-type", OFFSET_TYPE),
            ],
            bytes.to_vec(),
        )
        .await
    }

    /// Sends `bytes` in pieces of `size`.
    async fn send(app: &TestApp, session: &str, url: &str, bytes: &[u8], size: usize) {
        for (n, chunk) in bytes.chunks(size).enumerate() {
            let sent = piece(app, session, url, n * size, chunk).await;
            assert_eq!(sent.status, StatusCode::NO_CONTENT, "{}", sent.body);
            assert_eq!(
                sent.headers["upload-offset"],
                (n * size + chunk.len()).to_string()
            );
        }
    }

    async fn offset(app: &TestApp, session: &str, url: &str) -> TestResponse {
        app.request("HEAD", url, Some(session), &[TUS], Vec::new())
            .await
    }

    fn token(url: &str) -> &str {
        url.rsplit('/').next().unwrap()
    }

    /// Where piece `index` of the transfer at `url` is stored.
    fn stored_part(url: &str, index: u32) -> Key {
        let hash = moekura_core::tokens::hash_token(token(url));
        Key::transfer_part(&hex::encode(hash), index)
    }

    /// Whether each of the first four pieces of the transfer at `url` is
    /// stored.
    async fn stored_parts(state: &AppState, url: &str) -> Vec<bool> {
        let mut found = Vec::new();
        for index in 0..4 {
            found.push(
                state
                    .storage
                    .exists(&stored_part(url, index))
                    .await
                    .unwrap(),
            );
        }
        found
    }

    async fn post_begin(
        app: &TestApp,
        session: Option<&str>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        app.request("POST", FILES, session, headers, Vec::new())
            .await
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn a_file_sent_in_pieces_is_staged_by_the_form(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(40, 30);

        let options = app.request("OPTIONS", FILES, None, &[], Vec::new()).await;
        assert_eq!(options.status, StatusCode::NO_CONTENT);
        assert_eq!(options.headers["tus-version"], TUS_VERSION);
        assert_eq!(options.headers["tus-extension"], "creation,termination");
        assert_eq!(
            options.headers["tus-max-size"],
            max_bytes(&state).to_string()
        );

        let url = begin(&app, &alice, png.len(), "cat.png").await;
        assert!(url.starts_with("/uploads/files/"), "{url}");
        let head = offset(&app, &alice, &url).await;
        assert_eq!(head.status, StatusCode::OK);
        assert_eq!(head.headers["upload-offset"], "0");
        assert_eq!(head.headers["upload-length"], png.len().to_string());
        assert_eq!(head.headers["cache-control"], "no-store");

        // Pieces go in order, of the protocol's type.
        let size = png.len().div_ceil(3);
        let early = piece(&app, &alice, &url, size, &png[size..]).await;
        assert_eq!(early.status, StatusCode::CONFLICT, "{}", early.body);
        assert_eq!(early.headers["upload-offset"], "0");
        let untyped = app
            .request(
                "PATCH",
                &url,
                Some(&alice),
                &[TUS, ("upload-offset", "0")],
                png.clone(),
            )
            .await;
        assert_eq!(untyped.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        send(&app, &alice, &url, &png, size).await;
        assert_eq!(
            offset(&app, &alice, &url).await.headers["upload-offset"],
            png.len().to_string()
        );
        assert_eq!(stored_parts(&state, &url).await, [true, true, true, false]);
        // Nothing goes past the file's end.
        let past = piece(&app, &alice, &url, png.len(), b"x").await;
        assert_eq!(past.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", past.body);
        // Pieces waiting aren't served.
        let served = app
            .get(&format!("/data/{}", stored_part(&url, 0)), Some(&alice))
            .await;
        assert_eq!(served.status, StatusCode::NOT_FOUND);

        // Another user can't touch it.
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        assert_eq!(offset(&app, &bob, &url).await.status, StatusCode::NOT_FOUND);
        assert_eq!(
            piece(&app, &bob, &url, png.len(), b"x").await.status,
            StatusCode::NOT_FOUND
        );

        // The form names it instead of sending it.
        let sent = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[("transfer", token(&url).to_owned())],
                &[],
            )
            .await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let upload: i64 = sent
            .location
            .as_deref()
            .and_then(|l| l.strip_prefix("/uploads/"))
            .and_then(|id| id.parse().ok())
            .unwrap();
        let files = staged_uploads::of_upload(&pool, upload).await.unwrap();
        assert_eq!(
            files
                .iter()
                .map(|f| (f.file_name.as_str(), f.status, f.width))
                .collect::<Vec<_>>(),
            [("cat.png", staged_uploads::Status::Ready, Some(40))]
        );
        // Used up: gone, with its pieces.
        assert_eq!(
            offset(&app, &alice, &url).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(stored_parts(&state, &url).await, [false; 4]);
        let again = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[("transfer", token(&url).to_owned())],
                &[],
            )
            .await;
        assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(again.body.contains("wasn&#x27;t found"), "{}", again.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn files_sent_in_pieces_and_files_sent_whole_go_together(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let (a, b) = (fixture::png(40, 30), fixture::png(50, 30));
        let url = begin(&app, &alice, b.len(), "second.png").await;
        send(&app, &alice, &url, &b, 100).await;
        let sent = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[("transfer", token(&url).to_owned())],
                &[("file", "first.png", &a)],
            )
            .await;
        assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
        let upload: i64 = sent.location.unwrap()["/uploads/".len()..].parse().unwrap();
        let files = staged_uploads::of_upload(&pool, upload).await.unwrap();
        assert_eq!(
            files
                .iter()
                .map(|f| (f.file_name.as_str(), f.width))
                .collect::<Vec<_>>(),
            // In the form's order: the test client sends fields first.
            [("second.png", Some(50)), ("first.png", Some(40))]
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_one_step_form_takes_a_file_sent_in_pieces(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(24, 20);
        let url = begin(&app, &alice, png.len(), "whole.png").await;
        send(&app, &alice, &url, &png, 64).await;
        let fields = vec![
            ("transfer", token(&url).to_owned()),
            ("rating", "g".to_owned()),
            ("tags", "cat".to_owned()),
        ];
        let posted = app
            .post_multipart("/upload", Some(&alice), &fields, None)
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let id: i64 = posted.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let asset = moekura_db::media::for_post(&pool, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((asset.width, asset.height), (24, 20));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn an_unfinished_file_is_refused_by_the_form(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(40, 30);
        let url = begin(&app, &alice, png.len(), "half.png").await;
        send(&app, &alice, &url, &png[..png.len() / 2], 1024).await;
        let sent = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[("transfer", token(&url).to_owned())],
                &[],
            )
            .await;
        assert_eq!(sent.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            sent.body.contains("half.png wasn&#x27;t sent completely"),
            "{}",
            sent.body
        );
        assert_eq!(stored_parts(&state, &url).await, [false; 4]);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn refusals(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let alice = Some(session.as_str());
        let five = ("upload-length", "5");

        let visitor = post_begin(&app, None, &[TUS, five]).await;
        assert_eq!(visitor.status, StatusCode::UNAUTHORIZED);
        assert_eq!(visitor.body, "Log in to send files.");
        let purpose = format!("purpose {}", STANDARD.encode("delete"));
        let unknown = post_begin(&app, alice, &[TUS, five, ("upload-metadata", &purpose)]).await;
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
        // Members may search by image, but not replace posts' files.
        let purpose = format!("purpose {}", STANDARD.encode("replace"));
        let replace = post_begin(&app, alice, &[TUS, five, ("upload-metadata", &purpose)]).await;
        assert_eq!(replace.status, StatusCode::FORBIDDEN);

        let unversioned = post_begin(&app, alice, &[five]).await;
        assert_eq!(unversioned.status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(unversioned.headers["tus-version"], TUS_VERSION);

        let no_length = post_begin(&app, alice, &[TUS]).await;
        assert_eq!(no_length.status, StatusCode::BAD_REQUEST);
        let signed = post_begin(&app, alice, &[TUS, ("upload-length", "+5")]).await;
        assert_eq!(signed.status, StatusCode::BAD_REQUEST);

        let too_large = (max_bytes(&state) + 1).to_string();
        let large = post_begin(&app, alice, &[TUS, ("upload-length", &too_large)]).await;
        assert_eq!(large.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(large.body, "The file is larger than 100 MB.");

        let empty = post_begin(&app, alice, &[TUS, ("upload-length", "0")]).await;
        assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);

        // Only so many at once.
        for n in 0..transfers::MAX_PER_USER {
            begin(&app, &session, 5, &format!("{n}.png")).await;
        }
        let more = post_begin(&app, alice, &[TUS, five]).await;
        assert_eq!(more.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            more.body.contains("sending 40 files already"),
            "{}",
            more.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn given_up_and_idle_transfers_are_removed(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let png = fixture::png(40, 30);

        let cancelled = begin(&app, &alice, png.len(), "a.png").await;
        send(&app, &alice, &cancelled, &png[..200], 100).await;
        assert_eq!(
            stored_parts(&state, &cancelled).await,
            [true, true, false, false]
        );
        let gone = app
            .request("DELETE", &cancelled, Some(&alice), &[TUS], Vec::new())
            .await;
        assert_eq!(gone.status, StatusCode::NO_CONTENT);
        assert_eq!(stored_parts(&state, &cancelled).await, [false; 4]);
        assert_eq!(
            offset(&app, &alice, &cancelled).await.status,
            StatusCode::NOT_FOUND
        );

        let idle = begin(&app, &alice, png.len(), "b.png").await;
        send(&app, &alice, &idle, &png[..100], 100).await;
        let busy = begin(&app, &alice, png.len(), "c.png").await;
        send(&app, &alice, &busy, &png[..100], 100).await;
        sqlx::query(
            "UPDATE file_transfers SET updated_at = now() - interval '2 hours'
             WHERE token_hash = $1",
        )
        .bind(&moekura_core::tokens::hash_token(token(&idle))[..])
        .execute(&pool)
        .await
        .unwrap();
        prune(&state).await;
        assert_eq!(
            offset(&app, &alice, &idle).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(stored_parts(&state, &idle).await, [false; 4]);
        assert_eq!(offset(&app, &alice, &busy).await.status, StatusCode::OK);
        assert_eq!(
            stored_parts(&state, &busy).await,
            [true, false, false, false]
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn replacing_and_searching_take_files_sent_for_them(pool: PgPool) {
        let (app, state) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let boss = session_for(&pool, "boss", SystemRole::Moderator).await;
        let post = crate::danbooru::test_support::upload(&app, &alice, 20, "cat").await;

        // A moderator replaces the post's file with one sent in pieces.
        let bigger = fixture::png(40, 40);
        let url = begin_for(&app, &boss, bigger.len(), "bigger.png", "replace").await;
        send(&app, &boss, &url, &bigger, 100).await;
        let fields = [
            ("transfer", token(&url).to_owned()),
            ("reason", "Larger".to_owned()),
        ];
        let replaced = app
            .post_multipart(
                &format!("/posts/{post}/replace"),
                Some(&boss),
                &fields,
                None,
            )
            .await;
        assert_eq!(replaced.status, StatusCode::SEE_OTHER, "{}", replaced.body);
        let asset = moekura_db::media::for_post(&pool, post)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((asset.width, asset.height), (40, 40));
        assert_eq!(stored_parts(&state, &url).await, [false; 4]);

        // A file sent to search with isn't an upload's: the upload form
        // doesn't take it (it skipped the upload limits), and leaves it.
        let picture = fixture::png(64, 20);
        let url = begin_for(&app, &alice, picture.len(), "like.png", "search").await;
        send(&app, &alice, &url, &picture, 100).await;
        let transfer = [("transfer", token(&url).to_owned())];
        let uploaded = app
            .post_multipart_files("/uploads", Some(&alice), &transfer, &[])
            .await;
        assert_eq!(uploaded.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            uploaded.body.contains("wasn&#x27;t found"),
            "{}",
            uploaded.body
        );
        assert_eq!(offset(&app, &alice, &url).await.status, StatusCode::OK);
        // Nor does the replace form; the search does.
        let other = app
            .post_multipart(
                &format!("/posts/{post}/replace"),
                Some(&boss),
                &transfer,
                None,
            )
            .await;
        assert_eq!(other.status, StatusCode::UNPROCESSABLE_ENTITY);
        let searched = app
            .post_multipart("/iqdb_queries", Some(&alice), &transfer, None)
            .await;
        assert_eq!(searched.status, StatusCode::OK, "{}", searched.body);
        assert!(searched.body.contains("Posts like it"), "{}", searched.body);
        assert_eq!(
            offset(&app, &alice, &url).await.status,
            StatusCode::NOT_FOUND
        );

        // The API's search takes one too.
        let url = begin_for(&app, &alice, picture.len(), "like.png", "search").await;
        send(&app, &alice, &url, &picture, 100).await;
        let api = TestApp::new(state.clone(), crate::api::routes(max_bytes(&state)));
        let found = api
            .post_multipart(
                "/api/v1/posts/similar",
                Some(&alice),
                &[("transfer", token(&url).to_owned())],
                None,
            )
            .await;
        assert_eq!(found.status, StatusCode::OK, "{}", found.body);
    }

    #[test]
    fn file_names_come_from_the_metadata() {
        let name = |metadata: &str| file_name(metadata);
        let encoded = STANDARD.encode("猫.png");
        assert_eq!(name(&format!("filename {encoded}")), Some("猫.png".into()));
        assert_eq!(
            name(&format!("filetype aW1hZ2UvcG5n, filename {encoded}, empty")),
            Some("猫.png".into())
        );
        assert_eq!(name("filetype aW1hZ2UvcG5n"), None);
        assert_eq!(name("filename !!!"), None);
        assert_eq!(name("filename IA=="), None);
    }
}
