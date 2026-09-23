//! HTTP server for uwuubooru: HTML pages, the JSON API and operational
//! endpoints, all sharing one router.

mod health;

use std::future::Future;
use std::io;
use std::time::Duration;

use axum::Router;
use axum::http::{Request, StatusCode};
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::request_id::{
    MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::Span;
use uwuu_core::config::ServerConfig;
use uwuu_db::Db;
use uwuu_db::site_cache::SiteCache;

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    /// Site settings and roles, kept current across nodes.
    pub site: SiteCache,
}

pub fn router(state: AppState, config: &ServerConfig) -> Router {
    let middleware = ServiceBuilder::new()
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http().make_span_with(request_span))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(CatchPanicLayer::new())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(config.request_timeout_secs),
        ))
        .layer(CompressionLayer::new());

    Router::new()
        .merge(health::routes())
        .layer(middleware)
        .with_state(state)
}

fn request_span<B>(request: &Request<B>) -> Span {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .unwrap_or_default();
    tracing::info_span!(
        "request",
        method = %request.method(),
        path = request.uri().path(),
        request_id,
    )
}

/// Serves `router` on `listener` until `shutdown` resolves, then lets
/// in-flight requests finish.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}
