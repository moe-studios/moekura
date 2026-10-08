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
use moekura_core::user_settings::{Mode, UserSettings};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};

/// Everything a handler needs to render a page: extract it, then call
/// [`Page::render`].
pub struct Page {
    state: AppState,
    pub current: CurrentUser,
    notices: Notices,
    /// The request's path and query, for the layout.
    target: String,
}

/// What the layout takes from the request: its language, and what it
/// shows above the page.
#[derive(Debug, Clone, Default)]
pub(crate) struct Notices {
    /// The language tag pages are shown in.
    pub lang: String,
    pub flash: Option<Flash>,
    /// Site news is shown (it isn't on error pages).
    pub news: bool,
    /// The newest site news this browser dismissed.
    pub dismissed_news: Option<i64>,
}

/// The language to show `current` pages in, given the request's headers.
pub(crate) fn language(
    state: &AppState,
    current: Option<&CurrentUser>,
    headers: &axum::http::HeaderMap,
) -> String {
    let chosen = current
        .and_then(|c| c.user.as_ref())
        .and_then(|u| UserSettings::from_json(&u.settings).language);
    let accept = headers
        .get(axum::http::header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());
    state.locales.negotiate(chosen.as_deref(), accept)
}

impl FromRequestParts<AppState> for Page {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let current = CurrentUser::from_request_parts(parts, state).await?;
        let jar = CookieJar::from_headers(&parts.headers);
        let notices = Notices {
            lang: language(state, Some(&current), &parts.headers),
            flash: Flash::from_jar(&jar),
            news: true,
            dismissed_news: crate::news::dismissed(&jar),
        };
        let target = parts
            .uri
            .path_and_query()
            .map_or_else(|| "/".to_owned(), ToString::to_string);
        Ok(Self {
            state: state.clone(),
            current,
            notices,
            target,
        })
    }
}

impl Page {
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The language the page is shown in.
    pub(crate) fn lang(&self) -> &str {
        &self.notices.lang
    }

    /// Message `key` in the page's language (see [`crate::i18n::Locales::say`]).
    pub(crate) fn say(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.state.locales.say(self.lang(), key, args)
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
            self.notices.clone(),
            &self.target,
            status,
            template,
            context,
        );
        // The message has been shown; don't show it again.
        if self.notices.flash.is_some()
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
        "tags" | "wiki" | "artists" | "artist_versions" => "tags",
        "pools" => "pools",
        "comments" => "comments",
        "explore" => "explore",
        "forum_topics" | "forum_posts" => "forum",
        "upload" | "uploads" => "upload",
        "moderation" => "moderation",
        "admin" => "admin",
        "site_map" => "more",
        _ => return None,
    })
}

/// Renders `template` with the layout variables (`site`, `me`, `flash`)
/// merged into `context`. `target` is the request's path and query.
pub(crate) fn render(
    state: &AppState,
    current: Option<&CurrentUser>,
    notices: Notices,
    target: &str,
    status: StatusCode,
    template: &str,
    context: Value,
) -> Response {
    let site = state.site.get();
    let settings = &site.settings;
    let path = target.split('?').next().unwrap_or(target);
    let user_settings = current
        .and_then(|c| c.user.as_ref())
        .map(|user| UserSettings::from_json(&user.settings));
    let theme = crate::themes::resolve(
        &state.assets,
        user_settings.as_ref().and_then(|s| s.theme.as_deref()),
        &settings.default_theme,
    );
    let layout = context! {
        lang => notices.lang,
        // Messages for scripts, by key, `{$name}` left for them to fill.
        js_messages => state.locales.with_prefix(&notices.lang, "js-"),
        path => path,
        target => target,
        section => section(path),
        site => context! {
            name => settings.site_name,
            description => Some(&settings.site_description).filter(|d| !d.is_empty()),
            logo => crate::site::logo_url(state),
            full_logo => crate::site::full_logo_url(state),
            favicon => crate::site::favicon_url(state),
            hide_icon => settings.hide_header_icon,
            hide_name => settings.hide_header_name,
            has_rules => !settings.rules.trim().is_empty(),
            footer_links => settings.footer_links.iter().map(|link| context! {
                label => link.label,
                url => link.url,
                external => !link.url.starts_with('/'),
            }).collect::<Vec<_>>(),
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
            unread_messages => current.map_or(0, |c| c.unread_messages),
            unread_notifications => current.map_or(0, |c| c.unread_notifications),
            avatar => crate::profiles::image_url(state, current.and_then(|c| c.avatar_key.as_deref())),
        }),
        theme => theme,
        // The default theme is in the main stylesheet, without a file.
        theme_css => Some(format!("themes/{theme}.css"))
            .filter(|path| state.assets.url(path).is_some()),
        mode => user_settings
            .as_ref()
            .map(|s| s.mode)
            .filter(|mode| *mode != Mode::System)
            .map(Mode::as_str),
        // The user's choice, "system" included, for the footer switcher.
        mode_choice => user_settings.as_ref().map(|s| s.mode.as_str()),
        modes => Mode::ALL.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
        themes => crate::themes::choices(&state.assets),
        // Display settings; visitors get the defaults.
        prefs => {
            let prefs = user_settings.clone().unwrap_or_default();
            context! {
                large_thumbnails => prefs.large_thumbnails,
                autocomplete => prefs.autocomplete,
                shortcuts => prefs.shortcuts,
                custom_css => crate::users::custom_css_url(&prefs).map(|url| crate::templates::url_value(&url)),
            }
        },
        // Messages are HTML.
        flash => notices.flash.map(|f| {
            Value::from_safe_string(state.locales.format(&notices.lang, &format!("flash-{}", f.key()), None))
        }),
        news => notices.news.then(|| {
            crate::news::banner(&site, current.and_then(|c| c.user.as_ref()), notices.dismissed_news)
        }).flatten(),
        banned => current.and_then(|c| c.ban.as_ref()).map(|ban| context! {
            reason => ban.reason,
            until => ban.expires_at.map(crate::dates::day),
        }),
        can_upload => current.is_some_and(|c| c.can(Permission::Upload)),
        can_purge => current.is_some_and(|c| c.can(Permission::PurgePosts)),
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
    match crate::i18n::rendering_in(&notices.lang, || {
        state
            .templates
            .render(template, context! { ..context, ..layout })
    }) {
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
    async fn pages_follow_the_chosen_language(pool: PgPool) {
        let dir =
            std::env::temp_dir().join(format!("moekura-pages-locales-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("de")).unwrap();
        std::fs::write(
            dir.join("de/main.ftl"),
            "language-name = Deutsch\nnav-posts = Beiträge\nflash-saved = Gespeichert.\njs-close = Schließen\nerror-not-found = Nicht gefunden\n",
        )
        .unwrap();
        let mut config = crate::test_support::test_config();
        config.paths.locales_override = Some(dir.clone());
        let state = crate::test_support::test_state_with(&pool, config).await;
        let app = TestApp::new(state, crate::posts::routes().merge(crate::users::routes()));
        std::fs::remove_dir_all(&dir).unwrap();

        // Browsers ask.
        let german = app
            .get_with_headers("/", &[("accept-language", "fr, de-CH;q=0.8")])
            .await
            .body;
        assert!(german.contains("<html lang=\"de\""), "{german}");
        assert!(german.contains(">Beiträge</a>"), "{german}");
        // What German lacks is in English, and scripts get their messages.
        assert!(german.contains(">Tags</a>"));
        assert!(german.contains("Schließen"));
        let missing = app
            .get_with_headers("/nope", &[("accept-language", "de")])
            .await
            .body;
        assert!(missing.contains("Nicht gefunden"), "{missing}");
        let english = app.get("/", None).await.body;
        assert!(english.contains("<html lang=\"en-US\"") && english.contains(">Posts</a>"));

        // Users choose, whatever their browser says.
        let alice = crate::test_support::session_for(
            &pool,
            "alice",
            moekura_core::permissions::SystemRole::Member,
        )
        .await;
        let settings = app.get("/settings", Some(&alice)).await.body;
        assert!(
            settings.contains("<option value=\"de\">Deutsch</option>"),
            "{settings}"
        );
        let saved = app
            .post_form("/settings", Some(&alice), &[], "mode=system&language=de")
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER);
        assert!(
            app.get("/", Some(&alice))
                .await
                .body
                .contains(">Beiträge</a>")
        );
        assert_eq!(
            app.post_form("/settings", Some(&alice), &[], "mode=system&language=xx")
                .await
                .status,
            StatusCode::BAD_REQUEST
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
