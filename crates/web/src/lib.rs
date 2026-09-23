//! HTTP server for uwuubooru: HTML pages, the JSON API and operational
//! endpoints, all sharing one router.

pub mod auth;
pub mod error;
mod health;
#[cfg(test)]
mod test_support;

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{Request, StatusCode};
use axum::middleware;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::csrf::CsrfLayer;
use tower_http::request_id::{
    MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::Span;
use uwuu_core::config::Config;
use uwuu_db::Db;
use uwuu_db::site_cache::SiteCache;

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    /// Site settings and roles, kept current across nodes.
    pub site: SiteCache,
}

pub fn router(state: AppState) -> Router {
    with_middleware(Router::new(), state)
}

/// Wraps `routes` (the pages and API) in session handling and the global
/// middleware stack. Split out so tests can mount extra routes.
pub(crate) fn with_middleware(routes: Router<AppState>, state: AppState) -> Router {
    let server = &state.config.server;
    // Accept form posts from the public origin even when a proxy rewrites Host.
    let public_origin = server.public_url.origin().ascii_serialization();
    let csrf = CsrfLayer::new()
        .add_trusted_origin(&public_origin)
        .expect("an http(s) origin is a valid trusted origin");

    let middleware = ServiceBuilder::new()
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http().make_span_with(request_span))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(CatchPanicLayer::new())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(server.request_timeout_secs),
        ))
        .layer(CompressionLayer::new())
        .layer(csrf);

    routes
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::resolve_session,
        ))
        // Probes skip session handling.
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
    // Connection info gives handlers the peer address.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}
