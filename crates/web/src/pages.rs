//! Rendering HTML pages through the shared layout.

use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::header::SET_COOKIE;
use axum::http::request::Parts;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use uwuu_core::permissions::Permission;
use uwuu_core::settings::RegistrationMode;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(home))
}

async fn home(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    Ok(page.render("home.html", context! {}))
}

/// Everything a handler needs to render a page: extract it, then call
/// [`Page::render`].
pub struct Page {
    state: AppState,
    pub current: CurrentUser,
    flash: Option<Flash>,
}

impl FromRequestParts<AppState> for Page {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let current = CurrentUser::from_request_parts(parts, state).await?;
        let flash = Flash::from_jar(&CookieJar::from_headers(&parts.headers));
        Ok(Self {
            state: state.clone(),
            current,
            flash,
        })
    }
}

impl Page {
    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn render(&self, template: &str, context: Value) -> Response {
        self.render_with_status(StatusCode::OK, template, context)
    }

    pub fn render_with_status(
        &self,
        status: StatusCode,
        template: &str,
        context: Value,
    ) -> Response {
        let mut response = render(
            &self.state,
            Some(&self.current),
            self.flash,
            status,
            template,
            context,
        );
        // The message has been shown; don't show it again.
        if self.flash.is_some()
            && let Ok(value) = flash::removal().to_string().parse()
        {
            response.headers_mut().append(SET_COOKIE, value);
        }
        response
    }
}

/// Renders `template` with the layout variables (`site`, `me`, `flash`)
/// merged into `context`.
pub(crate) fn render(
    state: &AppState,
    current: Option<&CurrentUser>,
    flash: Option<Flash>,
    status: StatusCode,
    template: &str,
    context: Value,
) -> Response {
    let site = state.site.get();
    let settings = &site.settings;
    let layout = context! {
        site => context! {
            name => settings.site_name,
            registration_open => settings.registration_mode != RegistrationMode::Closed,
        },
        me => current.and_then(|c| c.user.as_ref()).map(|user| context! {
            name => user.name,
            role => current.map(|c| c.role.name.clone()),
        }),
        flash => flash.map(Flash::text),
    };
    match state
        .templates
        .render(template, context! { ..context, ..layout })
    {
        Ok(html) => (status, Html(html)).into_response(),
        Err(error) => {
            tracing::error!(%error, template, "template rendering failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong on our side",
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use sqlx::PgPool;
    use uwuu_core::permissions::Permissions;

    use crate::test_support::{TestApp, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(test_state(pool).await, super::routes())
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn home_renders_the_layout(pool: PgPool) {
        let response = app(&pool).await.get("/", None).await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(
            response.body.contains("<title>uwuubooru</title>"),
            "{}",
            response.body
        );
        assert!(response.body.contains("href=\"/login\""));
        assert!(response.body.contains("/static/css/main."));
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn private_sites_send_visitors_to_login(pool: PgPool) {
        // Take viewing away from logged-out visitors.
        sqlx::query("UPDATE roles SET permissions = $1 WHERE system_key = 'anonymous'")
            .bind(Permissions::NONE.to_db())
            .execute(&pool)
            .await
            .unwrap();
        let response = app(&pool).await.get("/?page=2", None).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        assert_eq!(
            response.location.as_deref(),
            Some("/login?next=%2F%3Fpage%3D2")
        );
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn unknown_pages_get_a_styled_404(pool: PgPool) {
        let response = app(&pool).await.get("/nope", None).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert!(response.body.contains("<html"), "{}", response.body);
        assert!(response.body.contains("Not found"));
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn security_headers_are_set(pool: PgPool) {
        let response = app(&pool).await.get_full("/").await;
        let csp = response
            .headers()
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            csp.contains("script-src 'self'") && csp.contains("frame-ancestors 'none'"),
            "{csp}"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn static_files_are_cached_forever(pool: PgPool) {
        let app = app(&pool).await;
        let home = app.get("/", None).await;
        let start = home.body.find("/static/css/main.").unwrap();
        let end = start + home.body[start..].find('"').unwrap();
        let css = app.get_full(&home.body[start..end]).await;
        assert_eq!(css.status(), StatusCode::OK);
        assert_eq!(
            css.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(css.headers()["content-type"], "text/css");
        assert_eq!(
            app.get_full("/static/css/main.css").await.status(),
            StatusCode::NOT_FOUND
        );
    }
}
