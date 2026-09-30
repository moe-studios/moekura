//! Answers for Danbooru features Moekura doesn't have, so clients carry on
//! instead of failing: their lists are empty (single items are already
//! 404, like any unknown URL).

use axum::Router;
use axum::response::Response;
use axum::routing::get;

use super::json;
use crate::AppState;
use crate::error::AppError;

/// Lists of things Moekura doesn't have.
pub(crate) const EMPTY_LISTS: &[&str] = &[
    "/forum_topics",
    "/forum_posts",
    "/forum_post_votes",
    "/dmails",
    "/user_feedbacks",
    "/user_name_change_requests",
    "/media_assets",
    "/iqdb_queries",
    "/users/{id}/uploads",
];

pub(super) fn routes() -> Router<AppState> {
    EMPTY_LISTS.iter().fold(Router::new(), |router, path| {
        router.route(path, get(nothing))
    })
}

async fn nothing() -> Result<Response, AppError> {
    json(Vec::<serde_json::Value>::new(), "")
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::app;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn missing_features_are_empty(pool: PgPool) {
        let app = app(&pool).await;
        for path in [
            "/dmails.json",
            "/forum_topics.json?search[id]=1",
            "/users/1/uploads.json",
        ] {
            let response = app.get(path, None).await;
            assert_eq!(response.status, StatusCode::OK, "{path}");
            assert_eq!(response.body, "[]", "{path}");
        }
        let missing = app.get("/dmails/1.json", None).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        let body: Value = serde_json::from_str(&missing.body).unwrap();
        assert_eq!(body["success"], json!(false));
    }
}
