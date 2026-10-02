//! Cross-origin access to the APIs (`[server.cors]`): answers browsers'
//! preflight requests and lets scripts on the allowed websites read the
//! answers.
//!
//! Requests from an allowed origin skip the cross-origin form check
//! (tower-http's CSRF layer), which would refuse their writes. Unless
//! `allow_credentials` is on, their cookies are dropped first, so they act
//! as a visitor or through an API key and can't ride on someone's login.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE,
    ACCESS_CONTROL_REQUEST_METHOD, COOKIE, ORIGIN, VARY,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use moekura_core::config::CorsConfig;
use tower::ServiceExt;
use tower_http::csrf::CsrfLayer;

/// Methods the APIs answer.
const METHODS: &str = "GET, HEAD, POST, PUT, PATCH, DELETE";
/// Request headers clients may send beyond the always-allowed ones.
const REQUEST_HEADERS: &str = "Authorization, Content-Type";
/// Response headers scripts may read beyond the always-readable ones.
const EXPOSED_HEADERS: &str = "X-RateLimit-Limit, X-RateLimit-Remaining, X-RateLimit-Reset, Retry-After, Location, X-Request-Id";

/// The allowed origins.
pub(crate) struct Cors {
    any: bool,
    origins: Vec<HeaderValue>,
    credentials: bool,
    max_age: HeaderValue,
    /// The site's own origin, whose requests are never cross-origin.
    own: HeaderValue,
}

/// Marks a request from an allowed origin.
#[derive(Clone, Copy)]
struct CrossOrigin;

impl Cors {
    /// `None` when no origin is allowed.
    pub(crate) fn new(config: &CorsConfig, own_origin: &str) -> Option<Arc<Self>> {
        if config.allowed_origins.is_empty() {
            return None;
        }
        Some(Arc::new(Self {
            any: config.allows_any(),
            origins: config
                .allowed_origins
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok())
                .collect(),
            credentials: config.allow_credentials,
            max_age: HeaderValue::from(config.max_age_secs),
            own: HeaderValue::from_str(own_origin).expect("an origin is a valid header"),
        }))
    }

    /// The `Origin` of a cross-origin request this allows.
    fn allowed_origin(&self, headers: &HeaderMap) -> Option<HeaderValue> {
        let origin = headers.get(ORIGIN)?;
        let same_site = headers
            .get("sec-fetch-site")
            .is_some_and(|v| v == "same-origin");
        (*origin != self.own
            && !same_site
            && (self.any || self.origins.iter().any(|o| o == origin)))
        .then(|| origin.clone())
    }

    fn allow(&self, headers: &mut HeaderMap, origin: HeaderValue) {
        if self.credentials {
            headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
            headers.insert(
                ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
        } else if self.any {
            headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        } else {
            headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        }
    }

    fn preflight(&self, origin: HeaderValue) -> Response {
        let mut response = StatusCode::NO_CONTENT.into_response();
        let headers = response.headers_mut();
        self.allow(headers, origin);
        headers.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(METHODS),
        );
        headers.insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(REQUEST_HEADERS),
        );
        headers.insert(ACCESS_CONTROL_MAX_AGE, self.max_age.clone());
        response
    }
}

/// Wraps `routes` in the cross-origin form check, skipped for API requests
/// from origins `cors` allows.
pub(crate) fn protect(routes: Router, csrf: CsrfLayer, cors: Option<Arc<Cors>>) -> Router {
    let checked = routes.clone().layer(csrf);
    let Some(cors) = cors else {
        return checked;
    };
    let pick = tower::service_fn(move |request: Request| {
        let target = if request.extensions().get::<CrossOrigin>().is_some() {
            routes.clone()
        } else {
            checked.clone()
        };
        async move { Ok::<_, Infallible>(target.oneshot(request).await.into_response()) }
    });
    Router::new()
        .fallback_service(pick)
        .layer(middleware::from_fn_with_state(cors, handle))
}

async fn handle(State(cors): State<Arc<Cors>>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if !(crate::api::is_api_path(path) || crate::danbooru::is_danbooru_path(path)) {
        return next.run(request).await;
    }
    let Some(origin) = cors.allowed_origin(request.headers()) else {
        let mut response = next.run(request).await;
        // Answers differ by origin, so caches mustn't mix them up.
        response
            .headers_mut()
            .append(VARY, HeaderValue::from_static("Origin"));
        return response;
    };
    if request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(ACCESS_CONTROL_REQUEST_METHOD)
    {
        let mut response = cors.preflight(origin);
        response
            .headers_mut()
            .append(VARY, HeaderValue::from_static("Origin"));
        return response;
    }
    request.extensions_mut().insert(CrossOrigin);
    if !cors.credentials {
        request.headers_mut().remove(COOKIE);
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    cors.allow(headers, origin);
    headers.insert(
        ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(EXPOSED_HEADERS),
    );
    headers.append(VARY, HeaderValue::from_static("Origin"));
    response
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, member, session_for, test_config, test_state_with};

    const APP: &str = "https://app.example.com";

    async fn app(pool: &PgPool, origins: &[&str], credentials: bool) -> TestApp {
        let mut config = test_config();
        config.server.cors.allowed_origins = origins.iter().map(|o| (*o).to_owned()).collect();
        config.server.cors.allow_credentials = credentials;
        let state = test_state_with(pool, config).await;
        TestApp::new(state.clone(), crate::all_routes(&state))
    }

    fn save_search(origin: &str, session: Option<&str>, key: Option<&str>) -> Request<Body> {
        let mut request = Request::post("/api/v1/saved-searches")
            .header("content-type", "application/json")
            .header("origin", origin)
            .header("sec-fetch-site", "cross-site");
        if let Some(session) = session {
            request = request.header("cookie", format!("moekura_session={session}"));
        }
        if let Some(key) = key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        request.body(Body::from(r#"{"query":"cat"}"#)).unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn answers_preflights_for_allowed_origins_only(pool: PgPool) {
        let app = app(&pool, &[APP], false).await;
        let preflight = |origin: &str, path: &str| {
            Request::builder()
                .method("OPTIONS")
                .uri(path)
                .header("origin", origin)
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    "authorization, content-type",
                )
                .body(Body::empty())
                .unwrap()
        };
        let response = app.raw(preflight(APP, "/api/v1/posts")).await;
        assert_eq!(response.status(), 204);
        let headers = response.headers();
        assert_eq!(headers["access-control-allow-origin"], APP);
        assert!(headers.get("access-control-allow-credentials").is_none());
        assert!(
            headers["access-control-allow-headers"]
                .to_str()
                .unwrap()
                .contains("Authorization")
        );
        assert_eq!(headers["access-control-max-age"], "600");

        // Danbooru-style URLs too.
        let response = app.raw(preflight(APP, "/posts.json")).await;
        assert_eq!(response.status(), 204);

        let other = app
            .raw(preflight("https://evil.example", "/api/v1/posts"))
            .await;
        assert!(other.headers().get("access-control-allow-origin").is_none());
        // Pages aren't part of it.
        let page = app.raw(preflight(APP, "/posts")).await;
        assert!(page.headers().get("access-control-allow-origin").is_none());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn allowed_origins_write_with_a_key_but_not_a_cookie(pool: PgPool) {
        let app = app(&pool, &[APP], false).await;
        let (alice, session) = member(&pool, "alice", "alice@example.com").await;
        let key = moekura_db::api_keys::create(&pool, alice.id, "app", None)
            .await
            .unwrap();

        let response = app.raw(save_search(APP, None, Some(&key))).await;
        assert_eq!(response.status(), 200);
        let headers = response.headers();
        assert_eq!(headers["access-control-allow-origin"], APP);
        assert!(
            headers["access-control-expose-headers"]
                .to_str()
                .unwrap()
                .contains("X-RateLimit-Remaining")
        );
        assert!(headers["x-ratelimit-remaining"].to_str().is_ok());

        // The session cookie is ignored: the request is a visitor's.
        let response = app.raw(save_search(APP, Some(&session), None)).await;
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["access-control-allow-origin"], APP);

        // Other origins still meet the cross-origin form check.
        let response = app
            .raw(save_search("https://evil.example", Some(&session), None))
            .await;
        assert_eq!(response.status(), 403);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn credentials_need_the_setting(pool: PgPool) {
        let app = app(&pool, &[APP], true).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let response = app.raw(save_search(APP, Some(&session), None)).await;
        assert_eq!(response.status(), 200);
        let headers = response.headers();
        assert_eq!(headers["access-control-allow-origin"], APP);
        assert_eq!(headers["access-control-allow-credentials"], "true");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn any_origin_reads_without_credentials(pool: PgPool) {
        let app = app(&pool, &["*"], false).await;
        let request = Request::get("/api/v1/posts")
            .header("origin", "https://anywhere.example")
            .body(Body::empty())
            .unwrap();
        let response = app.raw(request).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn off_by_default(pool: PgPool) {
        let state = crate::test_support::test_state(&pool).await;
        let app = TestApp::new(state.clone(), crate::all_routes(&state));
        let request = Request::get("/api/v1/posts")
            .header("origin", APP)
            .body(Body::empty())
            .unwrap();
        let response = app.raw(request).await;
        assert_eq!(response.status(), 200);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }
}
