//! Liveness and readiness probes for load balancers and orchestrators.

use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;

use crate::AppState;

const READY_TIMEOUT: Duration = Duration::from_secs(2);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(live))
        .route("/readyz", get(ready))
}

/// The process is up and serving HTTP. Does not touch dependencies, so a
/// database outage does not get every node restarted.
async fn live() -> &'static str {
    "ok"
}

/// The node can do useful work: the primary database answers in time.
async fn ready(State(state): State<AppState>) -> (StatusCode, &'static str) {
    match tokio::time::timeout(READY_TIMEOUT, state.db.ping()).await {
        Ok(Ok(())) => (StatusCode::OK, "ok"),
        Ok(Err(error)) => {
            tracing::warn!(%error, "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
        Err(_) => {
            tracing::warn!("readiness check timed out");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use sqlx::PgPool;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;
    use tower::ServiceExt;

    use uwuu_core::config::Config;
    use uwuu_core::settings::SiteSettings;
    use uwuu_db::Db;
    use uwuu_db::site_cache::{SiteCache, SiteSnapshot};

    use crate::{AppState, router};

    fn app(pool: PgPool) -> axum::Router {
        let state = AppState {
            config: Arc::new(Config::default()),
            db: Db::from_pools(pool, vec![]),
            site: SiteCache::from_snapshot(SiteSnapshot::new(SiteSettings::default(), vec![])),
        };
        router(state)
    }

    /// A pool pointing at a port nothing listens on.
    fn unreachable_pool() -> PgPool {
        PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(500))
            .connect_lazy("postgres://uwuu@127.0.0.1:1/uwuu")
            .unwrap()
    }

    async fn get(app: axum::Router, path: &str) -> (u16, String, bool) {
        let response = app
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let has_request_id = response.headers().contains_key("x-request-id");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            String::from_utf8(body.to_vec()).unwrap(),
            has_request_id,
        )
    }

    #[tokio::test]
    async fn live_does_not_need_database() {
        let (status, body, has_request_id) = get(app(unreachable_pool()), "/healthz").await;
        assert_eq!((status, body.as_str()), (200, "ok"));
        assert!(has_request_id);
    }

    #[tokio::test]
    async fn ready_fails_without_database() {
        let (status, _, _) = get(app(unreachable_pool()), "/readyz").await;
        assert_eq!(status, 503);
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn ready_succeeds_with_database(pool: PgPool) {
        let (status, body, _) = get(app(pool), "/readyz").await;
        assert_eq!((status, body.as_str()), (200, "ok"));
    }

    #[tokio::test]
    async fn unknown_paths_are_not_found() {
        let (status, _, _) = get(app(unreachable_pool()), "/nope").await;
        assert_eq!(status, 404);
    }
}
