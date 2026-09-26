//! A Danbooru-compatible API, so clients made for Danbooru (gallery-dl,
//! Grabber, Boorusama, …) work with a Moekura site.
//!
//! Danbooru's URLs end in `.json` (`/posts/123.json`), which the router
//! can't match with parameters, so [`rewrite`] moves such requests under
//! [`PREFIX`] before routing (`/__danbooru/posts/123`) and marks them with
//! [`DanbooruRequest`]. Credentials come as HTTP Basic or `login` and
//! `api_key` parameters (see [`credentials`]), and errors are answered in
//! Danbooru's shape ([`error_response`]).

mod ai_tags;
mod community;
mod missing;
mod notes;
mod posts;
mod reactions;
mod tags;
mod uploads;
mod users;

use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::Value;

use crate::AppState;
use crate::error::AppError;

/// Where Danbooru URLs are routed internally.
pub(crate) const PREFIX: &str = "/__danbooru";

/// Marks a request that came in on a Danbooru URL.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DanbooruRequest;

pub fn routes() -> Router<AppState> {
    Router::new().nest(
        PREFIX,
        posts::routes()
            .merge(ai_tags::routes())
            .merge(community::routes())
            .merge(missing::routes())
            .merge(notes::routes())
            .merge(reactions::routes())
            .merge(tags::routes())
            .merge(uploads::routes())
            .merge(users::routes()),
    )
}

/// Paths that end in `.json` without being Danbooru's.
fn ours(path: &str) -> bool {
    crate::api::is_api_path(path) || path.starts_with("/static/") || path.starts_with("/data/")
}

/// Moves a Danbooru URL (`/posts/1.json?tags=cat`) to its internal route
/// (`/__danbooru/posts/1?tags=cat`). Runs before routing.
pub(crate) fn rewrite(mut request: Request) -> Request {
    let path = request.uri().path();
    let Some(stem) = path.strip_suffix(".json").filter(|_| !ours(path)) else {
        return request;
    };
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    if let Ok(uri) = format!("{PREFIX}{stem}{query}").parse() {
        *request.uri_mut() = uri;
        request.extensions_mut().insert(DanbooruRequest);
    }
    request
}

pub(crate) fn is_danbooru_path(path: &str) -> bool {
    path == PREFIX || path.starts_with(&format!("{PREFIX}/"))
}

/// The name and API key a Danbooru client sent: HTTP Basic
/// `name:api_key` (gallery-dl), or `login` and `api_key` parameters
/// (Grabber, Boorusama).
pub(crate) fn credentials(request: &Request) -> Option<(String, String)> {
    let basic = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .and_then(|(_, encoded)| STANDARD.decode(encoded.trim()).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|pair| {
            let (name, key) = pair.split_once(':')?;
            Some((name.to_owned(), key.to_owned()))
        });
    basic
        .or_else(|| {
            let query = request.uri().query()?;
            let mut login = None;
            let mut key = None;
            for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
                match name.as_ref() {
                    "login" => login = Some(value.into_owned()),
                    "api_key" => key = Some(value.into_owned()),
                    _ => {}
                }
            }
            Some((login?, key?))
        })
        .filter(|(name, key)| !name.is_empty() && !key.is_empty())
}

/// Danbooru's error shape: `{"success": false, "error": …, "message": …}`.
pub(crate) fn error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({
        "success": false,
        "error": status.canonical_reason().unwrap_or("Error"),
        "message": message,
    });
    (status, axum::Json(body)).into_response()
}

/// Turns an error response from a handler into Danbooru's shape.
pub(crate) async fn error_response(response: Response) -> Response {
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    // The API's error rendering finds the message; reshape its body.
    let rendered = crate::error::json_error(response).await;
    let (parts, body) = rendered.into_parts();
    let message = match axum::body::to_bytes(body, 64 * 1024).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    let mut danbooru = error(status, &message);
    for (name, value) in &parts.headers {
        if name != CONTENT_TYPE && name != CONTENT_LENGTH {
            danbooru.headers_mut().append(name, value.clone());
        }
    }
    danbooru
}

/// Danbooru's timestamps: RFC 3339.
fn timestamp(at: time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Parameters from the query string and the body, form or JSON, as
/// Danbooru takes them. JSON objects are flattened to Danbooru's names:
/// `{"post": {"rating": "s"}}` is `post[rating]`.
#[derive(Debug, Default)]
pub(crate) struct Fields(std::collections::HashMap<String, String>);

impl Fields {
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    fn flatten(&mut self, prefix: Option<&str>, value: Value) {
        let name = |key: &str| match prefix {
            Some(prefix) => format!("{prefix}[{key}]"),
            None => key.to_owned(),
        };
        let Value::Object(map) = value else { return };
        for (key, value) in map {
            let name = name(&key);
            let flat = match value {
                Value::Object(_) => {
                    self.flatten(Some(&name), value);
                    continue;
                }
                Value::String(s) => s,
                Value::Null => String::new(),
                Value::Array(items) => items
                    .into_iter()
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                    .collect::<Vec<_>>()
                    .join(" "),
                other => other.to_string(),
            };
            self.0.insert(name, flat);
        }
    }
}

impl<S: Send + Sync> axum::extract::FromRequest<S> for Fields {
    type Rejection = AppError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let mut fields = Fields::default();
        if let Some(query) = request.uri().query() {
            fields
                .0
                .extend(url::form_urlencoded::parse(query.as_bytes()).into_owned());
        }
        let is_json = request
            .headers()
            .get(CONTENT_TYPE)
            .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
        let body = axum::body::Bytes::from_request(request, state)
            .await
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        if body.is_empty() {
            return Ok(fields);
        }
        if is_json {
            let value: Value = serde_json::from_slice(&body)
                .map_err(|e| AppError::BadRequest(format!("The body isn't JSON: {e}")))?;
            fields.flatten(None, value);
        } else {
            fields
                .0
                .extend(url::form_urlencoded::parse(&body).into_owned());
        }
        Ok(fields)
    }
}

/// Paging and field selection, as every Danbooru list takes them.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ListParams {
    #[serde(default)]
    limit: String,
    #[serde(default)]
    pub(crate) page: String,
    #[serde(default)]
    only: String,
}

impl ListParams {
    /// `limit`, defaulting to 20 as on Danbooru, at most `max`.
    fn limit(&self, max: u32) -> u32 {
        self.limit
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|&n| n > 0)
            .unwrap_or(20)
            .min(max)
    }
}

/// Keeps only the fields listed in `only` (`id,tag_string,file_url`) of
/// an object, or of each object in a list.
fn only(value: Value, only: &str) -> Value {
    let fields: Vec<&str> = only
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .collect();
    if fields.is_empty() {
        return value;
    }
    let trim = |value: Value| match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| fields.contains(&k.as_str()))
                .collect(),
        ),
        other => other,
    };
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(trim).collect()),
        other => trim(other),
    }
}

/// `value` as JSON, trimmed by `only`.
fn json(value: impl serde::Serialize, fields: &str) -> Result<Response, AppError> {
    let value = serde_json::to_value(value).map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(axum::Json(only(value, fields)).into_response())
}

#[cfg(test)]
pub(crate) mod test_support {
    use axum::Router;
    use sqlx::PgPool;

    use crate::AppState;
    use crate::test_support::{TestApp, test_state};

    /// The site's routes that Danbooru clients see, with the rewrite in
    /// front (as `with_middleware` puts it).
    pub(crate) async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(test_state(pool).await, routes())
    }

    /// Uploads a `width`×20 PNG as the user of `session`; returns the post.
    pub(crate) async fn upload(app: &TestApp, session: &str, width: u32, tags: &str) -> i64 {
        let fields = vec![("rating", "s".to_owned()), ("tags", tags.to_owned())];
        let file = crate::test_support::fixture::png(width, 20);
        let response = app
            .post_multipart("/upload", Some(session), &fields, Some(("a.png", &file)))
            .await;
        let location = response
            .location
            .unwrap_or_else(|| panic!("{}: {}", response.status, response.body));
        location["/posts/".len()..].parse().unwrap()
    }

    pub(crate) fn routes() -> Router<AppState> {
        super::routes()
            .merge(crate::upload::routes(100 * 1024 * 1024))
            .merge(crate::posts::routes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(uri: &str) -> Request {
        Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap()
    }

    #[test]
    fn rewrites_danbooru_urls() {
        let rewritten = rewrite(request("/posts/12.json?tags=cat+dog&page=b5"));
        assert_eq!(rewritten.uri(), "/__danbooru/posts/12?tags=cat+dog&page=b5");
        assert!(rewritten.extensions().get::<DanbooruRequest>().is_some());
        for path in [
            "/posts/12",
            "/api/v1/openapi.json",
            "/static/app.json",
            "/data/x.json",
        ] {
            let untouched = rewrite(request(path));
            assert_eq!(untouched.uri(), path);
            assert!(untouched.extensions().get::<DanbooruRequest>().is_none());
        }
    }

    #[test]
    fn reads_both_kinds_of_credentials() {
        let basic = Request::builder()
            .uri("/posts.json")
            .header(
                AUTHORIZATION,
                format!("Basic {}", STANDARD.encode("alice:mka_key")),
            )
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            credentials(&basic),
            Some(("alice".into(), "mka_key".into()))
        );
        let params = request("/posts.json?login=alice&api_key=mka_key&tags=x");
        assert_eq!(
            credentials(&params),
            Some(("alice".into(), "mka_key".into()))
        );
        assert_eq!(credentials(&request("/posts.json?login=alice")), None);
        assert_eq!(credentials(&request("/posts.json?login=&api_key=")), None);
    }

    #[test]
    fn trims_fields() {
        let value = serde_json::json!([{ "id": 1, "rating": "g", "md5": "x" }]);
        assert_eq!(
            only(value.clone(), "id, md5"),
            serde_json::json!([{ "id": 1, "md5": "x" }])
        );
        assert_eq!(only(value.clone(), ""), value);
    }
}
