//! The error type handlers return, and the middleware that turns errors
//! into HTML pages, or into JSON for the API.

use axum::extract::{Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use minijinja::context;

use crate::AppState;
use crate::auth::CurrentUser;

#[derive(Debug)]
pub enum AppError {
    NotFound,
    /// The visitor must log in first.
    Unauthorized,
    /// Logged in, but not allowed.
    Forbidden,
    /// Not allowed, with the reason shown (a ban).
    Blocked(String),
    BadRequest(String),
    TooManyRequests {
        retry_after_secs: u64,
    },
    /// Something broke on our side. The message is logged, never shown.
    Internal(String),
}

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden | AppError::Blocked(_) => StatusCode::FORBIDDEN,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::TooManyRequests { .. } => StatusCode::TOO_MANY_REQUESTS,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// What the visitor is told.
    pub fn public_message(&self) -> &str {
        match self {
            AppError::NotFound => "Not found",
            AppError::Unauthorized => "You need to log in first",
            AppError::Forbidden => "You don't have permission to do that",
            AppError::Blocked(message) => message,
            AppError::BadRequest(message) => message,
            AppError::TooManyRequests { .. } => {
                "Too many attempts. Please wait a moment and try again"
            }
            AppError::Internal(_) => "Something went wrong on our side",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if let AppError::Internal(detail) = &self {
            tracing::error!(error = %detail, "internal error");
        }
        let page = ErrorPage {
            status: self.status(),
            message: self.public_message().to_owned(),
        };
        // Plain text by default; `render_errors` upgrades it to a page.
        let mut response = (page.status, page.message.clone()).into_response();
        if let AppError::TooManyRequests { retry_after_secs } = self {
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(retry_after_secs));
        }
        response.extensions_mut().insert(page);
        response
    }
}

/// Marks a response as an error for [`render_errors`].
#[derive(Debug, Clone)]
struct ErrorPage {
    status: StatusCode,
    message: String,
}

/// Middleware: renders [`AppError`] responses as HTML pages. Visitors who
/// need to log in are sent to the login page and brought back afterwards.
/// Under `/api/` every error becomes JSON instead, see [`json_error`].
pub async fn render_errors(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if crate::api::is_api_path(request.uri().path()) {
        return json_error(next.run(request).await).await;
    }
    let current = request.extensions().get::<CurrentUser>().cloned();
    let is_get = request.method() == Method::GET;
    let target = request
        .uri()
        .path_and_query()
        .map_or_else(|| "/".to_owned(), ToString::to_string);

    let response = next.run(request).await;
    let Some(page) = response.extensions().get::<ErrorPage>().cloned() else {
        return response;
    };

    let mut rendered = if page.status == StatusCode::UNAUTHORIZED && is_get {
        let next: String = url::form_urlencoded::byte_serialize(target.as_bytes()).collect();
        Redirect::to(&format!("/login?next={next}")).into_response()
    } else {
        let context = context! { status => page.status.as_u16(), message => page.message };
        crate::pages::render(
            &state,
            current.as_ref(),
            None,
            page.status,
            "error.html",
            context,
        )
    };
    // Keep headers the handler set (cookies, Retry-After), but not ones
    // describing the plain-text body we replaced.
    for (name, value) in response.headers() {
        if name != CONTENT_TYPE && name != CONTENT_LENGTH {
            rendered.headers_mut().append(name, value.clone());
        }
    }
    rendered
}

/// The API's error body.
#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct ErrorDetail {
    /// The HTTP status code again.
    pub status: u16,
    /// What went wrong, for people.
    pub message: String,
}

/// Turns an error response into an [`ErrorBody`]: [`AppError`]s keep their
/// message, and anything else (a malformed request body or query rejected
/// before the handler ran) keeps its plain-text explanation.
pub(crate) async fn json_error(response: Response) -> Response {
    let status = response.status();
    let is_json = response
        .headers()
        .get(CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
    if !(status.is_client_error() || status.is_server_error()) || is_json {
        return response;
    }
    let (parts, body) = response.into_parts();
    let message = match parts.extensions.get::<ErrorPage>() {
        Some(page) => page.message.clone(),
        None => match axum::body::to_bytes(body, 16 * 1024).await {
            Ok(bytes) if !bytes.is_empty() => String::from_utf8_lossy(&bytes).trim().to_owned(),
            _ => status.canonical_reason().unwrap_or("Error").to_owned(),
        },
    };
    let body = ErrorBody {
        error: ErrorDetail {
            status: status.as_u16(),
            message,
        },
    };
    let mut rendered = (status, axum::Json(body)).into_response();
    for (name, value) in &parts.headers {
        if name != CONTENT_TYPE && name != CONTENT_LENGTH {
            rendered.headers_mut().append(name, value.clone());
        }
    }
    if status == StatusCode::UNAUTHORIZED && !rendered.headers().contains_key(WWW_AUTHENTICATE) {
        rendered
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    rendered
}

pub async fn not_found() -> AppError {
    AppError::NotFound
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        AppError::Internal(format!("database: {error}"))
    }
}
