//! Serving stored post files at `/data/<key>` when no CDN or public bucket
//! does it (`storage.public_base_url` unset).

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_SECURITY_POLICY,
    CONTENT_TYPE, RANGE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use uwuu_storage::{GetRange, Key, StorageError};

use crate::AppState;
use crate::error::AppError;

/// Files are content-addressed, so a URL's bytes never change.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Even if a stored file were somehow opened as a document, it can't run
/// scripts or act as a page of this site.
const FILE_CSP: &str = "default-src 'none'; img-src 'self'; media-src 'self'; sandbox";

pub async fn serve(
    State(state): State<AppState>,
    Path(raw_key): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !state.storage.served_by_app() {
        return Err(AppError::NotFound);
    }
    let key = Key::parse(&raw_key).ok_or(AppError::NotFound)?;
    let requested = match headers.get(RANGE).map(|v| v.to_str().map(parse_range)) {
        None => None,
        Some(Ok(Some(range))) => Some(range),
        // Malformed or multi-range: serve the whole file, as RFC 9110 allows.
        Some(_) => None,
    };

    let file = match state.storage.read(&key, requested.clone()).await {
        Ok(file) => file,
        Err(StorageError::NotFound) => return Err(AppError::NotFound),
        Err(StorageError::InvalidRange) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(ACCEPT_RANGES, "bytes")],
            )
                .into_response());
        }
        Err(error) => return Err(AppError::Internal(error.to_string())),
    };

    let content_type = mime_guess::from_ext(key.extension()).first_or_octet_stream();
    let length = file.range.end - file.range.start;
    let status = if requested.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut response = (status, Body::from_stream(file.stream)).into_response();
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, header(content_type.essence_str()));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(FILE_CSP));
    if requested.is_some() {
        let content_range = format!(
            "bytes {}-{}/{}",
            file.range.start,
            file.range.end.saturating_sub(1),
            file.total_size
        );
        headers.insert(CONTENT_RANGE, header(&content_range));
    }
    Ok(response)
}

fn header(value: &str) -> HeaderValue {
    HeaderValue::from_str(value)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

/// A single `bytes=` range: `a-b`, `a-` or `-n`.
fn parse_range(value: &str) -> Option<GetRange> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    match (start.trim(), end.trim()) {
        ("", n) => n.parse().ok().filter(|&n| n > 0).map(GetRange::Suffix),
        (a, "") => a.parse().ok().map(GetRange::Offset),
        (a, b) => {
            let (a, b): (u64, u64) = (a.parse().ok()?, b.parse().ok()?);
            (a <= b).then(|| GetRange::Bounded(a..b + 1))
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use sqlx::PgPool;
    use tower::ServiceExt;
    use uwuu_storage::Key;

    use super::*;
    use crate::test_support::test_state;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_single_ranges() {
        assert!(matches!(parse_range("bytes=0-99"), Some(GetRange::Bounded(r)) if r == (0..100)));
        assert!(matches!(
            parse_range("bytes=100-"),
            Some(GetRange::Offset(100))
        ));
        assert!(matches!(
            parse_range("bytes=-500"),
            Some(GetRange::Suffix(500))
        ));
        for bad in [
            "bytes=5-1",
            "bytes=0-1,5-9",
            "items=0-1",
            "bytes=-0",
            "bytes=a-b",
            "bytes=",
        ] {
            assert!(parse_range(bad).is_none(), "{bad}");
        }
    }

    async fn get(
        app: &Router,
        path: &str,
        range: Option<&str>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut request = Request::get(path);
        if let Some(range) = range {
            request = request.header(RANGE, range);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes().to_vec();
        (parts.status, parts.headers, bytes)
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn serves_whole_files_and_ranges(pool: PgPool) {
        let state = test_state(&pool).await;
        let key = Key::original(HASH, "png");
        state
            .storage
            .put_bytes(&key, b"0123456789".to_vec().into())
            .await
            .unwrap();
        let app = crate::with_middleware(Router::new(), state);
        let path = format!("/data/{key}");

        let (status, headers, body) = get(&app, &path, None).await;
        assert_eq!(
            (status, body.as_slice()),
            (StatusCode::OK, &b"0123456789"[..])
        );
        assert_eq!(headers[CONTENT_TYPE], "image/png");
        assert_eq!(headers[CACHE_CONTROL], IMMUTABLE);
        assert!(
            headers[CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("sandbox")
        );
        assert_eq!(headers[ACCEPT_RANGES], "bytes");

        let (status, headers, body) = get(&app, &path, Some("bytes=2-4")).await;
        assert_eq!(
            (status, body.as_slice()),
            (StatusCode::PARTIAL_CONTENT, &b"234"[..])
        );
        assert_eq!(headers[CONTENT_RANGE], "bytes 2-4/10");
        assert_eq!(headers[CONTENT_LENGTH], "3");

        let (status, _, body) = get(&app, &path, Some("bytes=-2")).await;
        assert_eq!(
            (status, body.as_slice()),
            (StatusCode::PARTIAL_CONTENT, &b"89"[..])
        );

        let (status, _, _) = get(&app, &path, Some("bytes=50-")).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);

        // Unparseable ranges fall back to the whole file.
        let (status, _, body) = get(&app, &path, Some("bytes=9-1")).await;
        assert_eq!((status, body.len()), (StatusCode::OK, 10));
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn refuses_unknown_and_malformed_keys(pool: PgPool) {
        let app = crate::with_middleware(Router::new(), test_state(&pool).await);
        let missing = format!("/data/original/01/23/{HASH}.png");
        assert_eq!(get(&app, &missing, None).await.0, StatusCode::NOT_FOUND);
        for bad in [
            "/data/../Cargo.toml",
            "/data/original/01/23/nothex.png",
            "/data/x",
        ] {
            assert_eq!(get(&app, bad, None).await.0, StatusCode::NOT_FOUND, "{bad}");
        }
    }
}
