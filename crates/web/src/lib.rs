//! HTTP server for uwubooru: HTML pages, the JSON API and operational
//! endpoints, all sharing one router.

mod account;
mod admin;
pub mod api;
mod assets;
pub mod auth;
mod bans;
mod blacklist;
mod client_ip;
mod edit;
pub mod error;
mod favorites;
mod fetch;
mod files;
pub mod flash;
mod health;
mod history;
mod moderation;
pub mod pages;
mod posts;
pub mod rate_limit;
mod tag_relations;
mod tags;
mod templates;
#[cfg(test)]
mod test_support;
mod upload;
mod users;

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::header::{CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderValue, Request, StatusCode};
use axum::middleware;
use axum::routing::get;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::csrf::CsrfLayer;
use tower_http::request_id::{
    MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::Span;
use uwu_core::config::Config;
use uwu_db::Db;
use uwu_db::site_cache::SiteCache;

use crate::assets::Assets;
use crate::rate_limit::RateLimits;
use crate::templates::Templates;
use uwu_media::Media;
use uwu_storage::Storage;

/// Scripts and styles only from our own origin, images and video also from
/// the file storage's public origin (a CDN) if there is one; no framing, no
/// plugins, forms only to ourselves.
fn content_security_policy(storage_origin: Option<&str>) -> String {
    let files = storage_origin.map(|o| format!(" {o}")).unwrap_or_default();
    format!(
        "default-src 'self'; img-src 'self' data: blob:{files}; media-src 'self' blob:{files}; \
         style-src 'self'; script-src 'self'; object-src 'none'; base-uri 'none'; \
         frame-ancestors 'none'; form-action 'self'"
    )
}

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    /// Site settings and roles, kept current across nodes.
    pub site: SiteCache,
    pub rate_limits: Arc<RateLimits>,
    pub storage: Storage,
    pub media: Media,
    pub(crate) fetcher: fetch::Fetcher,
    /// Scratch space for uploads in progress.
    pub(crate) work_dir: std::path::PathBuf,
    pub(crate) file_signer: files::FileSigner,
    templates: Arc<Templates>,
    assets: Arc<Assets>,
}

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("could not load static files: {0}")]
    Assets(#[from] io::Error),
    #[error("could not load templates: {0:#}")]
    Templates(#[from] minijinja::Error),
    #[error("could not create the media work directory: {0}")]
    WorkDir(io::Error),
    #[error("could not open file storage: {0}")]
    Storage(#[from] uwu_storage::StorageError),
}

impl AppState {
    /// Loads static files and compiles templates, honouring the override
    /// directories in `config.paths`.
    /// `file_key` signs file URLs on private sites; every node needs the
    /// same one (`uwu_db::secrets`).
    pub fn new(
        config: Config,
        db: Db,
        site: SiteCache,
        file_key: [u8; 32],
    ) -> Result<Self, StartupError> {
        let storage = Storage::from_config(&config.storage)?;
        let work_dir = config.media.work_dir_or_default();
        std::fs::create_dir_all(&work_dir).map_err(StartupError::WorkDir)?;
        let assets = Arc::new(Assets::load(config.paths.static_override.as_deref())?);
        let templates = Arc::new(Templates::load(
            config.paths.templates_override.clone(),
            assets.clone(),
        )?);
        let media = Media::new(config.media.clone());
        Ok(Self {
            config: Arc::new(config),
            db,
            site,
            rate_limits: Arc::new(RateLimits::default()),
            storage,
            media,
            fetcher: fetch::Fetcher::new(std::time::Duration::from_secs(120), false),
            work_dir,
            file_signer: files::FileSigner::new(file_key),
            templates,
            assets,
        })
    }
}

impl AppState {
    /// Whether visitors are kept out (the Anonymous role can't view
    /// posts). Files then need signed URLs.
    pub fn is_private(&self) -> bool {
        self.site
            .get()
            .system_role(uwu_core::permissions::SystemRole::Anonymous)
            .is_none_or(|role| !role.can(uwu_core::permissions::Permission::ViewPosts))
    }

    /// The URL browsers load a stored file from, signed on private sites
    /// when this server serves the files.
    pub fn file_url(&self, key: &uwu_storage::Key) -> String {
        let url = self.storage.url(key);
        if self.storage.served_by_app() && self.is_private() {
            let now = time::OffsetDateTime::now_utc().unix_timestamp();
            format!("{url}{}", self.file_signer.query(key.as_str(), now))
        } else {
            url
        }
    }
}

pub fn router(state: AppState) -> Router {
    let max_upload_bytes = state.config.media.max_upload_mb * 1024 * 1024;
    let routes = posts::routes()
        .merge(api::routes())
        .merge(account::routes())
        .merge(admin::routes())
        .merge(bans::routes())
        .merge(edit::routes())
        .merge(favorites::routes())
        .merge(history::routes())
        .merge(moderation::routes())
        .merge(tags::routes())
        .merge(tag_relations::routes())
        .merge(users::routes())
        .merge(upload::routes(max_upload_bytes));
    with_middleware(routes, state)
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
        .layer(csrf)
        .layer(SetResponseHeaderLayer::if_not_present(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&content_security_policy(
                state.storage.public_origin().as_deref(),
            ))
            .expect("an ASCII origin makes a valid header"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ));

    routes
        .fallback(error::not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::block_banned_networks,
        ))
        // Inner layer: runs after the session is known, so error pages can
        // show who is logged in.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            error::render_errors,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::resolve_session,
        ))
        // Probes and static files skip session handling.
        .merge(health::routes())
        .route("/static/{*path}", get(assets::serve))
        .route("/data/{*key}", get(files::serve))
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

#[cfg(test)]
mod tests {
    use super::content_security_policy;

    #[test]
    fn csp_allows_media_from_the_storage_origin() {
        let own = content_security_policy(None);
        assert!(own.contains("img-src 'self' data: blob:;"), "{own}");
        let cdn = content_security_policy(Some("https://cdn.example.com"));
        assert!(
            cdn.contains("img-src 'self' data: blob: https://cdn.example.com;"),
            "{cdn}"
        );
        assert!(
            cdn.contains("media-src 'self' blob: https://cdn.example.com;"),
            "{cdn}"
        );
        assert!(
            cdn.contains("script-src 'self';"),
            "scripts stay local: {cdn}"
        );
    }
}
