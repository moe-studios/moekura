//! Rendering HTML pages through the shared layout.

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::header::SET_COOKIE;
use axum::http::request::Parts;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::settings::RegistrationMode;
use moekura_core::user_settings::{Theme, UserSettings};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};

/// Everything a handler needs to render a page: extract it, then call
/// [`Page::render`].
pub struct Page {
    state: AppState,
    pub current: CurrentUser,
    flash: Option<Flash>,
    /// The request's path and query, for the layout.
    target: String,
}

impl FromRequestParts<AppState> for Page {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let current = CurrentUser::from_request_parts(parts, state).await?;
        let flash = Flash::from_jar(&CookieJar::from_headers(&parts.headers));
        let target = parts
            .uri
            .path_and_query()
            .map_or_else(|| "/".to_owned(), ToString::to_string);
        Ok(Self {
            state: state.clone(),
            current,
            flash,
            target,
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
            &self.target,
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

/// Which main menu item a path belongs under, for `aria-current`.
fn section(path: &str) -> Option<&'static str> {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    Some(match first {
        "" | "posts" => "posts",
        "tags" | "wiki" => "tags",
        "pools" => "pools",
        "comments" => "comments",
        "upload" => "upload",
        "moderation" => "moderation",
        "admin" => "admin",
        _ => return None,
    })
}

/// Renders `template` with the layout variables (`site`, `me`, `flash`)
/// merged into `context`. `target` is the request's path and query.
pub(crate) fn render(
    state: &AppState,
    current: Option<&CurrentUser>,
    flash: Option<Flash>,
    target: &str,
    status: StatusCode,
    template: &str,
    context: Value,
) -> Response {
    let site = state.site.get();
    let settings = &site.settings;
    let path = target.split('?').next().unwrap_or(target);
    let layout = context! {
        path => path,
        target => target,
        section => section(path),
        site => context! {
            name => settings.site_name,
            registration_open => settings.registration_mode != RegistrationMode::Closed,
        },
        // Preset reasons for deleting, rejecting and flagging posts.
        reasons => context! {
            deletion => settings.post_reasons.deletion,
            flag => settings.post_reasons.flag,
        },
        me => current.and_then(|c| c.user.as_ref()).map(|user| context! {
            name => user.name,
            role => current.map(|c| c.role.name.clone()),
        }),
        theme => current
            .and_then(|c| c.user.as_ref())
            .map(|user| UserSettings::from_json(&user.settings).theme)
            .filter(|theme| *theme != Theme::System)
            .map(Theme::as_str),
        // The user's choice, "system" included, for the footer switcher.
        theme_choice => current
            .and_then(|c| c.user.as_ref())
            .map(|user| UserSettings::from_json(&user.settings).theme.as_str()),
        themes => Theme::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        flash => flash.map(Flash::text),
        banned => current.and_then(|c| c.ban.as_ref()).map(|ban| context! {
            reason => ban.reason,
            until => ban.expires_at.map(|t| t.date().to_string()),
        }),
        can_upload => current.is_some_and(|c| c.can(Permission::Upload)),
        can_admin => current.is_some_and(|c| {
            c.can(Permission::ManageSettings) || c.can(Permission::ManageUsers)
        }),
        moderation_url => current.and_then(|c| {
            if c.can(Permission::ApprovePosts) {
                Some("/moderation/queue")
            } else if c.can(Permission::ViewAuditLog) {
                Some("/moderation/log")
            } else {
                None
            }
        }),
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
    use moekura_core::permissions::Permissions;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(test_state(pool).await, crate::posts::routes())
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn home_renders_the_layout(pool: PgPool) {
        let response = app(&pool).await.get("/", None).await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(
            response.body.contains("<title>Moekura</title>"),
            "{}",
            response.body
        );
        assert!(response.body.contains("href=\"/login\""));
        assert!(response.body.contains("/static/css/main."));
        assert!(
            response
                .body
                .contains("<script type=\"module\" src=\"/static/js/main.")
        );
    }

    #[test]
    fn paths_map_to_menu_sections() {
        assert_eq!(super::section("/"), Some("posts"));
        assert_eq!(super::section("/posts/12"), Some("posts"));
        assert_eq!(super::section("/wiki/long_hair"), Some("tags"));
        assert_eq!(super::section("/tags/aliases"), Some("tags"));
        assert_eq!(super::section("/moderation/queue"), Some("moderation"));
        assert_eq!(super::section("/settings"), None);
        assert_eq!(super::section("/postscript"), None);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn unknown_pages_get_a_styled_404(pool: PgPool) {
        let response = app(&pool).await.get("/nope", None).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert!(response.body.contains("<html"), "{}", response.body);
        assert!(response.body.contains("Not found"));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
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
