//! HTTP server for Moekura: HTML pages, the JSON API and operational
//! endpoints, all sharing one router.

mod account;
mod admin;
pub mod api;
mod api_keys;
mod artists;
mod assets;
pub mod auth;
mod bans;
mod blacklist;
mod captcha;
mod client_ip;
mod comments;
mod counts;
mod danbooru;
mod dates;
mod edit;
mod email;
pub mod error;
pub mod explore;
mod favorite_groups;
mod favorites;
mod feeds;
mod fetch;
mod files;
pub mod flash;
mod health;
mod history;
pub mod import;
mod mass_edit;
mod metatags;
mod moderation;
mod notes;
pub mod oidc;
pub mod pages;
mod pools;
mod post_history;
mod posts;
mod previews;
pub mod rate_limit;
mod recent_changes;
mod related_tags;
pub mod remote_import;
mod requests;
mod saved_searches;
pub mod shared;
mod site;
mod suggestions;
mod tag_history;
mod tag_relations;
mod tag_warnings;
mod tags;
mod templates;
#[cfg(test)]
mod test_support;
mod themes;
mod two_factor;
mod upload;
mod user_moderation;
mod users;
mod webhooks;
mod wiki;

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
use moekura_core::config::Config;
use moekura_db::Db;
use moekura_db::site_cache::SiteCache;
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

use crate::assets::Assets;
use crate::rate_limit::RateLimits;
use crate::templates::Templates;
use moekura_media::Media;
use moekura_storage::Storage;

/// Scripts and styles only from our own origin, images and video also from
/// the file storage's public origin (a CDN) if there is one; no framing, no
/// plugins, forms only to ourselves. A captcha service's widget may load
/// its scripts, styles and frames from `captcha_origins`.
fn content_security_policy(storage_origin: Option<&str>, captcha_origins: &[&str]) -> String {
    let files = storage_origin.map(|o| format!(" {o}")).unwrap_or_default();
    let captcha: String = captcha_origins.iter().map(|o| format!(" {o}")).collect();
    let frames = if captcha.is_empty() {
        String::new()
    } else {
        format!("frame-src 'self'{captcha}; connect-src 'self'{captcha}; ")
    };
    format!(
        "default-src 'self'; img-src 'self' data: blob:{files}; media-src 'self' blob:{files}; \
         style-src 'self'{captcha}; script-src 'self'{captcha}; {frames}object-src 'none'; \
         base-uri 'none'; frame-ancestors 'none'; form-action 'self'"
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
    pub(crate) counts: Arc<counts::CountCache>,
    /// Post views and searches not yet written to the database.
    pub(crate) tallies: Arc<explore::Tallies>,
    pub storage: Storage,
    pub media: Media,
    pub(crate) fetcher: fetch::Fetcher,
    /// Scratch space for uploads in progress.
    pub(crate) work_dir: std::path::PathBuf,
    pub(crate) file_signer: files::FileSigner,
    /// The single sign-on provider, if there is one.
    pub(crate) oidc: Option<Arc<oidc::Oidc>>,
    /// The captcha service, if there is one.
    pub(crate) captcha: Option<Arc<captcha::Captcha>>,
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
    Storage(#[from] moekura_storage::StorageError),
    #[error("cache.url: {0}")]
    Cache(#[from] redis::RedisError),
}

impl AppState {
    /// Loads static files and compiles templates, honouring the override
    /// directories in `config.paths`.
    /// `file_key` signs file URLs on private sites; every node needs the
    /// same one (`moekura_db::secrets`).
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
        let valkey = match (&config.cache.backend, &config.cache.url) {
            (moekura_core::config::CacheBackend::Valkey, Some(url)) => {
                Some(shared::Valkey::new(url, &config.cache.prefix)?)
            }
            _ => None,
        };
        let counts = counts::CountCache::new(
            Duration::from_secs(config.cache.count_ttl_secs),
            valkey.clone(),
        );
        let oidc = config
            .auth
            .oidc
            .clone()
            .map(|oidc| Arc::new(oidc::Oidc::new(oidc, &config.server.public_url)));
        let captcha = config
            .auth
            .captcha
            .clone()
            .map(|c| Arc::new(captcha::Captcha::new(c)));
        let rate_limits = RateLimits::new(valkey).with_api_limit(
            config.server.api_requests_per_minute,
            config.server.api_burst,
        );
        Ok(Self {
            config: Arc::new(config),
            db,
            site,
            rate_limits: Arc::new(rate_limits),
            counts: Arc::new(counts),
            tallies: Arc::default(),
            storage,
            media,
            fetcher: fetch::Fetcher::new(std::time::Duration::from_secs(120), false),
            work_dir,
            file_signer: files::FileSigner::new(file_key),
            oidc,
            captcha,
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
            .system_role(moekura_core::permissions::SystemRole::Anonymous)
            .is_none_or(|role| !role.can(moekura_core::permissions::Permission::ViewPosts))
    }

    /// The pool for a replica-safe read by `current`: a replica, unless they
    /// changed something moments ago and must see it.
    pub fn reader(&self, current: &auth::CurrentUser) -> &sqlx::PgPool {
        if current.recent_write {
            self.db.primary()
        } else {
            self.db.read()
        }
    }

    /// The URL browsers load a stored file from, signed on private sites
    /// when this server serves the files.
    pub fn file_url(&self, key: &moekura_storage::Key) -> String {
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
        .merge(api::routes(max_upload_bytes))
        .merge(api_keys::routes())
        .merge(artists::routes())
        .merge(danbooru::routes())
        .merge(account::routes())
        .merge(admin::routes())
        .merge(bans::routes())
        .merge(comments::routes())
        .merge(edit::routes())
        .merge(email::routes())
        .merge(explore::routes())
        .merge(favorite_groups::routes())
        .merge(favorites::routes())
        .merge(feeds::routes())
        .merge(history::routes())
        .merge(mass_edit::routes())
        .merge(moderation::routes())
        .merge(notes::routes())
        .merge(oidc::routes())
        .merge(pools::routes())
        .merge(post_history::routes())
        .merge(previews::routes())
        .merge(recent_changes::routes())
        .merge(related_tags::routes())
        .merge(requests::routes())
        .merge(saved_searches::routes())
        .merge(site::routes())
        .merge(tags::routes())
        .merge(tag_history::routes())
        .merge(tag_relations::routes())
        .merge(two_factor::routes())
        .merge(user_moderation::routes())
        .merge(users::routes())
        .merge(webhooks::routes())
        .merge(wiki::routes())
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
                state
                    .config
                    .auth
                    .captcha
                    .as_ref()
                    .map_or(&[], |c| c.provider.origins()),
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

    let routes = routes
        .fallback(error::not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::block_banned_networks,
        ))
        // Knows the requester, and its refusals become JSON errors.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit::limit_api,
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
        .with_state(state);
    // Before routing, which layers on the router run after.
    let danbooru_urls = tower::util::MapRequestLayer::new(danbooru::rewrite);
    Router::new().fallback_service(tower::Layer::layer(&danbooru_urls, routes))
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
        let own = content_security_policy(None, &[]);
        assert!(own.contains("img-src 'self' data: blob:;"), "{own}");
        let cdn = content_security_policy(Some("https://cdn.example.com"), &[]);
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
        assert!(!cdn.contains("frame-src"), "{cdn}");
        let captcha = content_security_policy(None, &["https://challenges.cloudflare.com"]);
        for directive in ["script-src", "frame-src", "style-src"] {
            assert!(
                captcha.contains(&format!(
                    "{directive} 'self' https://challenges.cloudflare.com;"
                )),
                "{captcha}"
            );
        }
    }
}
