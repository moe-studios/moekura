//! The error type handlers return.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug)]
pub enum AppError {
    NotFound,
    /// The visitor must log in first.
    Unauthorized,
    /// Logged in, but not allowed.
    Forbidden,
    BadRequest(String),
    /// Something broke on our side. The message is logged, never shown.
    Internal(String),
}

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// What the visitor is told.
    pub fn public_message(&self) -> &str {
        match self {
            AppError::NotFound => "Not found",
            AppError::Unauthorized => "You need to log in first",
            AppError::Forbidden => "You don't have permission to do that",
            AppError::BadRequest(message) => message,
            AppError::Internal(_) => "Something went wrong on our side",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if let AppError::Internal(detail) = &self {
            tracing::error!(error = %detail, "internal error");
        }
        (self.status(), self.public_message().to_owned()).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        AppError::Internal(format!("database: {error}"))
    }
}
