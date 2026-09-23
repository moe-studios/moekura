//! The JSON API under `/api/v1`.
//!
//! Handlers apply the same permissions and rules as the pages, usually by
//! calling the same functions. Errors are JSON ([`crate::error::ErrorBody`],
//! rendered by [`crate::error::render_errors`]). The OpenAPI description is
//! generated from the handlers' annotations, so it can't drift from them.

mod posts;
mod tags;
mod users;

use std::sync::Arc;

use axum::Router;
use axum::http::header::CONTENT_TYPE;
use axum::routing::get;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{OpenApi as Spec, SecurityRequirement};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::SESSION_COOKIE;
use crate::error::ErrorBody;

/// Where the API is mounted.
pub const BASE: &str = "/api/v1";

pub fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "uwubooru API",
        description = "Read and change a uwubooru site: search and upload posts, edit tags, \
                       moderate. Responses are JSON; errors look like \
                       `{\"error\": {\"status\": 404, \"message\": \"Not found\"}}`.\n\n\
                       Send an API key (created in your account settings) as \
                       `Authorization: Bearer <key>`. Without one, requests are made as a \
                       logged-out visitor and can do what visitors can on the site.",
        license(name = "AGPL-3.0-only", identifier = "AGPL-3.0-only"),
    ),
    servers((url = "/api/v1")),
    modifiers(&Auth),
    tags(
        (name = "posts", description = "Searching, viewing and changing posts."),
        (name = "tags", description = "Tags, their categories, aliases and implications."),
        (name = "users", description = "Users and the account making the request."),
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

fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(posts::search))
        .routes(routes!(posts::show))
        .routes(routes!(posts::versions))
        .routes(routes!(tags::list))
        .routes(routes!(tags::show))
        .routes(routes!(tags::autocomplete))
        .routes(routes!(tags::relations))
        .routes(routes!(users::show))
        .routes(routes!(users::me))
}

/// The API's OpenAPI description.
pub fn openapi() -> Spec {
    api_router().into_openapi()
}

pub fn routes() -> Router<AppState> {
    let (router, spec) = api_router().split_for_parts();
    let spec: Arc<str> = spec
        .to_pretty_json()
        .expect("the OpenAPI description serializes")
        .into();
    let router = router.route(
        "/openapi.json",
        get(move || async move { ([(CONTENT_TYPE, "application/json")], spec.to_string()) }),
    );
    Router::new().nest(BASE, router)
}

/// `url` as an absolute URL: stored files are usually served from a path
/// on this site, which API clients can't resolve on their own.
fn absolute_url(state: &AppState, url: &str) -> String {
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

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn errors_are_json(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
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

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn serves_its_openapi_description(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let response = app.get("/api/v1/openapi.json", None).await;
        assert_eq!(response.status, StatusCode::OK);
        let spec: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert!(spec["openapi"].as_str().unwrap().starts_with("3."));
        assert!(spec["paths"]["/posts/{id}"]["get"].is_object(), "{spec}");
        assert!(spec["components"]["securitySchemes"]["api_key"].is_object());
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
        let routes = super::routes()
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
        location["/posts/".len()..].parse().unwrap()
    }

    pub fn json(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
    }
}
