//! The JSON API under `/api/v1`.
//!
//! Handlers apply the same permissions and rules as the pages, usually by
//! calling the same functions. Errors are JSON ([`crate::error::ErrorBody`],
//! rendered by [`crate::error::render_errors`]). The OpenAPI description is
//! generated from the handlers' annotations, so it can't drift from them.

mod comments;
mod docs;
mod favorite_groups;
mod moderation;
mod notes;
mod pools;
pub(crate) mod posts;
mod saved_searches;
mod tags;
mod users;
mod wiki;

use std::sync::Arc;

use axum::Router;
use axum::extract::{MatchedPath, Request};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{OpenApi as Spec, RefOr, SecurityRequirement};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use minijinja::context;

use crate::AppState;
use crate::auth::SESSION_COOKIE;
use crate::error::ErrorBody;
use crate::pages::Page;

/// Where the API is mounted.
pub const BASE: &str = "/api/v1";

/// The API reference page.
pub const DOCS: &str = "/api/docs";

pub fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Moekura API",
        description = "Read and change a Moekura site: search and upload posts, edit tags, \
                       moderate. Responses are JSON; errors look like \
                       `{\"error\": {\"status\": 404, \"message\": \"Not found\"}}`.\n\n\
                       Send an API key (created in your account settings) as \
                       `Authorization: Bearer <key>`. Without one, requests are made as a \
                       logged-out visitor and can do what visitors can on the site.\n\n\
                       Requests are rate limited per account (or, without a key, per \
                       address). Responses carry `X-RateLimit-Limit`, `X-RateLimit-Remaining` \
                       and `X-RateLimit-Reset` (Unix time when the full burst is back); past \
                       the limit, requests get a 429 with `Retry-After`.",
        license(name = "AGPL-3.0-only", identifier = "AGPL-3.0-only"),
    ),
    servers((url = "/api/v1")),
    modifiers(&Auth),
    tags(
        (name = "posts", description = "Searching, viewing and changing posts."),
        (name = "tags", description = "Tags, their categories, aliases and implications."),
        (name = "users", description = "Users and the account making the request."),
        (name = "wiki", description = "Wiki pages about tags, and their history."),
        (name = "comments", description = "Comments on posts, their votes and reports."),
        (name = "pools", description = "Ordered collections of posts, and their history."),
        (name = "notes", description = "Notes (translations) on posts' images, and their history."),
        (name = "moderation", description = "Reviewing posts and flags, deciding tag relations, bans \
                                             and the moderation log."),
    ),
    components(schemas(ErrorBody)),
)]
struct ApiDoc;

/// Adds the ways to authenticate. Every operation accepts either, or none
/// (as a visitor); which permission it needs is in its description.
struct Auth;

impl Modify for Auth {
    fn modify(&self, spec: &mut Spec) {
        let components = spec.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some("An API key from your account settings."))
                    .build(),
            ),
        );
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                SESSION_COOKIE,
                "The site's login session, for scripts running on its pages.",
            ))),
        );
        spec.security = Some(vec![
            SecurityRequirement::default(),
            SecurityRequirement::new("api_key", Vec::<String>::new()),
            SecurityRequirement::new("session", Vec::<String>::new()),
        ]);
    }
}

/// The API's routes; uploads may be up to `max_upload_bytes`.
fn api_router(max_upload_bytes: u64) -> OpenApiRouter<AppState> {
    let uploads = OpenApiRouter::new()
        .routes(routes!(posts::search, posts::upload))
        .routes(routes!(posts::similar))
        .layer(crate::upload::body_limit(max_upload_bytes));
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(uploads)
        .routes(routes!(posts::show, posts::update))
        .routes(routes!(posts::versions))
        .routes(routes!(posts::favorite, posts::unfavorite))
        .routes(routes!(posts::vote))
        .routes(routes!(posts::flag))
        .routes(routes!(tags::list))
        .routes(routes!(tags::show, tags::update))
        .routes(routes!(tags::autocomplete))
        .routes(routes!(tags::relations, tags::request))
        .routes(routes!(wiki::list))
        .routes(routes!(wiki::show, wiki::save))
        .routes(routes!(wiki::versions))
        .routes(routes!(comments::list))
        .routes(routes!(comments::show, comments::update, comments::delete))
        .routes(routes!(comments::create))
        .routes(routes!(comments::vote))
        .routes(routes!(comments::report))
        .routes(routes!(comments::hide))
        .routes(routes!(comments::restore))
        .routes(routes!(notes::list, notes::create))
        .routes(routes!(notes::versions))
        .routes(routes!(notes::show, notes::update, notes::delete))
        .routes(routes!(pools::list, pools::create))
        .routes(routes!(pools::show, pools::update, pools::delete))
        .routes(routes!(pools::add_post))
        .routes(routes!(pools::restore))
        .routes(routes!(pools::versions))
        .routes(routes!(saved_searches::list, saved_searches::create))
        .routes(routes!(saved_searches::update, saved_searches::delete))
        .routes(routes!(favorite_groups::list, favorite_groups::create))
        .routes(routes!(
            favorite_groups::show,
            favorite_groups::update,
            favorite_groups::delete
        ))
        .routes(routes!(favorite_groups::add_post))
        .routes(routes!(favorite_groups::remove_post))
        .routes(routes!(users::show))
        .routes(routes!(users::me))
        .routes(routes!(moderation::approve))
        .routes(routes!(moderation::reject))
        .routes(routes!(moderation::disapprove))
        .routes(routes!(moderation::appeal))
        .routes(routes!(moderation::delete))
        .routes(routes!(moderation::restore))
        .routes(routes!(moderation::purge))
        .routes(routes!(moderation::delete_uploads))
        .routes(routes!(moderation::purge_posts))
        .routes(routes!(moderation::post_batch))
        .routes(routes!(moderation::dismiss_flags))
        .routes(routes!(moderation::open_flags))
        .routes(routes!(moderation::approve_relation))
        .routes(routes!(moderation::reject_relation))
        .routes(routes!(moderation::remove_relation))
        .routes(routes!(moderation::list_bans))
        .routes(routes!(moderation::ban_user, moderation::unban_user))
        .routes(routes!(moderation::ban_network))
        .routes(routes!(moderation::lift_network_ban))
        .routes(routes!(moderation::log))
}

/// An operation kept working for old clients after being replaced. It's
/// marked `deprecated` in the OpenAPI description, and its responses carry
/// a `Deprecation` header (RFC 9745). It's removed in the next major
/// release; see docs/src/stability.md.
///
/// A deprecated field is marked with `#[schema(deprecated)]` instead, and
/// keeps being sent.
pub(crate) struct Deprecated {
    pub method: Method,
    /// As in the OpenAPI description: `/posts/{id}`.
    pub path: &'static str,
    /// When it was deprecated, as Unix time.
    pub since: i64,
    /// What to use instead, for its description.
    pub instead: &'static str,
}

/// Deprecated operations, oldest first.
const DEPRECATED: &[Deprecated] = &[];

/// Marks deprecated operations in the description. Panics on one that
/// doesn't exist, so a typo fails the tests.
fn mark_deprecated(mut spec: Spec, deprecated: &[Deprecated]) -> Spec {
    for entry in deprecated {
        let operation = spec
            .paths
            .paths
            .get_mut(entry.path)
            .and_then(|item| match entry.method {
                Method::GET => item.get.as_mut(),
                Method::PUT => item.put.as_mut(),
                Method::POST => item.post.as_mut(),
                Method::DELETE => item.delete.as_mut(),
                Method::PATCH => item.patch.as_mut(),
                _ => None,
            });
        let Some(operation) = operation else {
            panic!(
                "deprecated {} {} isn't an operation",
                entry.method, entry.path
            );
        };
        operation.deprecated = Some(utoipa::openapi::Deprecated::True);
        let note = format!(
            "**Deprecated:** use {} instead. This keeps working until the next major release.",
            entry.instead
        );
        operation.description = Some(match operation.description.take() {
            Some(description) => format!("{note}\n\n{description}"),
            None => note,
        });
    }
    spec
}

/// Adds `Deprecation` to responses from deprecated operations.
async fn deprecation_header(
    deprecated: &'static [Deprecated],
    request: Request,
    next: Next,
) -> Response {
    let since = request
        .extensions()
        .get::<MatchedPath>()
        .and_then(|matched| matched.as_str().strip_prefix(BASE))
        .and_then(|path| {
            deprecated
                .iter()
                .find(|d| d.path == path && d.method == request.method())
        })
        .map(|d| d.since);
    let mut response = next.run(request).await;
    if let Some(since) = since {
        let value = HeaderValue::from_str(&format!("@{since}")).expect("digits");
        response.headers_mut().insert("deprecation", value);
    }
    response
}

/// Gives every response a description, which OpenAPI requires: the
/// status's reason phrase where the handler didn't say more.
fn complete(mut spec: Spec) -> Spec {
    for item in spec.paths.paths.values_mut() {
        let operations = [
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.patch,
        ];
        for operation in operations.into_iter().flatten() {
            for (status, response) in &mut operation.responses.responses {
                if let RefOr::T(response) = response
                    && response.description.is_empty()
                {
                    response.description = status
                        .parse::<u16>()
                        .ok()
                        .and_then(|s| StatusCode::from_u16(s).ok())
                        .and_then(|s| s.canonical_reason())
                        .unwrap_or("Response")
                        .to_owned();
                }
            }
        }
    }
    spec
}

/// The API's OpenAPI description.
pub fn openapi() -> Spec {
    mark_deprecated(complete(api_router(0).into_openapi()), DEPRECATED)
}

pub fn routes(max_upload_bytes: u64) -> Router<AppState> {
    routes_with(max_upload_bytes, DEPRECATED)
}

fn routes_with(max_upload_bytes: u64, deprecated: &'static [Deprecated]) -> Router<AppState> {
    let (mut router, spec) = api_router(max_upload_bytes).split_for_parts();
    let spec = mark_deprecated(complete(spec), deprecated);
    if !deprecated.is_empty() {
        router = router.route_layer(middleware::from_fn(move |request, next| {
            deprecation_header(deprecated, request, next)
        }));
    }
    let json: Arc<str> = spec
        .to_pretty_json()
        .expect("the OpenAPI description serializes")
        .into();
    let reference =
        docs::reference(&serde_json::to_value(&spec).expect("the OpenAPI description serializes"));
    let router = router.route(
        "/openapi.json",
        get(move || async move { ([(CONTENT_TYPE, "application/json")], json.to_string()) }),
    );
    Router::new().nest(BASE, router).route(
        DOCS,
        get(move |page: Page| async move {
            let base = absolute_url(page.state(), BASE);
            page.render(
                "api_docs.html",
                context! { api => reference, base => base, spec_url => format!("{BASE}/openapi.json") },
            )
        }),
    )
}

/// `url` as an absolute URL: stored files are usually served from a path
/// on this site, which API clients can't resolve on their own.
pub(crate) fn absolute_url(state: &AppState, url: &str) -> String {
    if url.starts_with('/') {
        state
            .config
            .server
            .public_url
            .join(url)
            .map_or_else(|_| url.to_owned(), String::from)
    } else {
        url.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn errors_are_json(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes(1024));
        let response = app.get("/api/v1/nope", None).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"error": {"status": 404, "message": "Not found"}})
        );

        // Rejected before the handler ran.
        let response = app.get("/api/v1/posts/abc", None).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Invalid URL"),
            "{body}"
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn serves_its_openapi_description(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes(1024));
        let response = app.get("/api/v1/openapi.json", None).await;
        assert_eq!(response.status, StatusCode::OK);
        let spec: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert!(spec["openapi"].as_str().unwrap().starts_with("3."));
        assert!(spec["paths"]["/posts/{id}"]["get"].is_object(), "{spec}");
        assert!(spec["components"]["securitySchemes"]["api_key"].is_object());
        // OpenAPI requires every response to be described.
        let created = &spec["paths"]["/posts"]["post"]["responses"]["201"];
        assert_eq!(created["description"], "Created");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn deprecated_operations_are_marked(pool: PgPool) {
        const DEPRECATED: &[super::Deprecated] = &[super::Deprecated {
            method: axum::http::Method::GET,
            path: "/me",
            since: 1_790_000_000,
            instead: "`GET /users/{name}`",
        }];
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes_with(1024, DEPRECATED),
        );
        let response = app.get("/api/v1/me", None).await;
        assert_eq!(response.headers.get("deprecation").unwrap(), "@1790000000");
        let other = app.get("/api/v1/tags", None).await;
        assert!(other.headers.get("deprecation").is_none());

        let spec: serde_json::Value =
            serde_json::from_str(&app.get("/api/v1/openapi.json", None).await.body).unwrap();
        let operation = &spec["paths"]["/me"]["get"];
        assert_eq!(operation["deprecated"], true);
        assert!(
            operation["description"]
                .as_str()
                .unwrap()
                .starts_with("**Deprecated:** use `GET /users/{name}` instead."),
            "{operation}"
        );
        assert!(spec["paths"]["/tags"]["get"]["deprecated"].is_null());
    }

    /// What OpenAPI linters check that the annotations could get wrong.
    #[test]
    fn operations_are_unique_and_described() {
        let spec = serde_json::to_value(super::openapi()).unwrap();
        let mut ids = std::collections::HashSet::new();
        for (path, item) in spec["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                let id = operation["operationId"].as_str().unwrap();
                assert!(ids.insert(id.to_owned()), "{id} is used twice");
                assert!(
                    operation["summary"].is_string(),
                    "{method} {path} has no summary"
                );
                for (status, response) in operation["responses"].as_object().unwrap() {
                    assert!(
                        response["description"]
                            .as_str()
                            .is_some_and(|d| !d.is_empty()),
                        "{method} {path} {status} has no description"
                    );
                }
            }
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn renders_a_reference_page(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes(1024));
        let page = app.get("/api/docs", None).await;
        assert_eq!(page.status, StatusCode::OK);
        let body = &page.body;
        assert!(body.contains("id=\"search_posts\""), "{body}");
        assert!(body.contains("<code>view_posts</code>"), "{body}");
        assert!(body.contains("href=\"#schema-ApiPost\""), "{body}");
        assert!(body.contains("id=\"schema-ApiPost\""), "{body}");
        assert!(body.contains("Authorization: Bearer"), "{body}");
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use sqlx::PgPool;

    use crate::test_support::{TestApp, test_state};

    /// The API plus the pages that create things for it to show.
    pub async fn app(pool: &PgPool) -> TestApp {
        let state = test_state(pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let routes = super::routes(max)
            .merge(crate::upload::routes(max))
            .merge(crate::edit::routes());
        TestApp::new(state, routes)
    }

    /// Uploads `png` through the site's form and returns the post's id.
    pub async fn upload(app: &TestApp, session: &str, png: &[u8], tags: &str) -> i64 {
        let fields = vec![("rating", "s".to_owned()), ("tags", tags.to_owned())];
        let response = app
            .post_multipart("/upload", Some(session), &fields, Some(("a.png", png)))
            .await;
        let location = response
            .location
            .unwrap_or_else(|| panic!("upload failed: {}", response.body));
        location["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    pub fn json(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
    }
}
