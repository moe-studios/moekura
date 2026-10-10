//! The site's own pages and identity: the rules page, the site map, and
//! the logos and favicon.

use axum::Router;
use axum::extract::{DefaultBodyLimit, Multipart};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_db::mod_actions::{self, NewAction};
use moekura_media::MediaType;
use moekura_storage::Key;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

/// The largest logo, full logo or favicon accepted.
const IMAGE_MAX_BYTES: usize = 1024 * 1024;

/// The site's images an admin uploads from the settings page.
#[derive(Clone, Copy)]
enum SiteImage {
    /// The icon beside the site's name in the header.
    Logo,
    /// A whole logo in place of the icon and the name.
    FullLogo,
    /// The browser tab's icon.
    Favicon,
}

impl SiteImage {
    /// The setting holding its storage key, which is also the form field.
    fn key(self) -> &'static str {
        match self {
            Self::Logo => "logo",
            Self::FullLogo => "full_logo",
            Self::Favicon => "favicon",
        }
    }

    /// The storage prefix its files go under.
    fn prefix(self) -> &'static str {
        match self {
            Self::Logo => "logo",
            Self::FullLogo => "full-logo",
            Self::Favicon => "favicon",
        }
    }

    /// What it's called in messages.
    fn name(self) -> &'static str {
        match self {
            Self::Logo => "The icon",
            Self::FullLogo => "The logo",
            Self::Favicon => "The favicon",
        }
    }

    fn stored(self, settings: &moekura_core::settings::SiteSettings) -> &str {
        match self {
            Self::Logo => &settings.logo,
            Self::FullLogo => &settings.full_logo,
            Self::Favicon => &settings.favicon,
        }
    }
}

pub fn routes() -> Router<AppState> {
    let limit = || DefaultBodyLimit::max(IMAGE_MAX_BYTES + 16 * 1024);
    Router::new()
        .route("/rules", get(rules))
        .route(
            "/admin/settings/logo",
            post(|page: Page, jar: CookieJar, form: Multipart| {
                set_image(page, jar, form, SiteImage::Logo)
            })
            .layer(limit()),
        )
        .route(
            "/admin/settings/full-logo",
            post(|page: Page, jar: CookieJar, form: Multipart| {
                set_image(page, jar, form, SiteImage::FullLogo)
            })
            .layer(limit()),
        )
        .route(
            "/admin/settings/favicon",
            post(|page: Page, jar: CookieJar, form: Multipart| {
                set_image(page, jar, form, SiteImage::Favicon)
            })
            .layer(limit()),
        )
        .route("/site_map", get(site_map))
}

/// Where browsers load `image` from, if the site has one.
fn image_url(state: &AppState, image: SiteImage) -> Option<Value> {
    let site = state.site.get();
    let key = Key::parse(image.stored(&site.settings))?;
    Some(crate::templates::url_value(&state.file_url(&key)))
}

/// Where browsers load the header's icon from, if the site has one.
pub fn logo_url(state: &AppState) -> Option<Value> {
    image_url(state, SiteImage::Logo)
}

/// Where browsers load the full logo from, if the site has one.
pub fn full_logo_url(state: &AppState) -> Option<Value> {
    image_url(state, SiteImage::FullLogo)
}

/// Where browsers load the uploaded favicon from, if the site has one.
pub fn favicon_url(state: &AppState) -> Option<Value> {
    image_url(state, SiteImage::Favicon)
}

/// Every page on the site, by area, for finding the ones the menus leave
/// out.
async fn site_map(page: Page) -> Response {
    page.render("site_map.html", context! {})
}

/// The content rules. Anyone may read them, on private sites too, since
/// sign-up links to them.
async fn rules(page: Page) -> Result<Response, AppError> {
    let rules = page.state().site.get().settings.rules.clone();
    if rules.trim().is_empty() {
        return Err(AppError::NotFound);
    }
    Ok(page.render(
        "rules.html",
        context! {
            html => Value::from_safe_string(markup::render(&rules)),
            can_edit => page.current.can(Permission::ManageSettings),
        },
    ))
}

/// Replaces one of the site's images with an upload, or removes it
/// (`remove`).
async fn set_image(
    page: Page,
    jar: CookieJar,
    mut form: Multipart,
    image: SiteImage,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let too_big = || AppError::BadRequest(format!("{} must be at most 1 MB", image.name()));
    let mut upload: Option<Vec<u8>> = None;
    let mut remove = false;
    while let Some(field) = form.next_field().await.map_err(|_| too_big())? {
        match field.name() {
            Some(name) if name == image.key() => {
                let bytes = field.bytes().await.map_err(|_| too_big())?;
                if !bytes.is_empty() {
                    upload = Some(bytes.to_vec());
                }
            }
            Some("remove") => remove = true,
            _ => {}
        }
    }
    let value = match (upload, remove) {
        (_, true) => String::new(),
        (Some(bytes), false) => {
            let kind = MediaType::sniff(&bytes)
                .filter(|kind| {
                    matches!(
                        kind,
                        MediaType::Png
                            | MediaType::Jpeg
                            | MediaType::Gif
                            | MediaType::Webp
                            | MediaType::Avif
                    )
                })
                .ok_or_else(|| {
                    AppError::Unprocessable(format!(
                        "{} must be a PNG, JPEG, GIF, WebP or AVIF image",
                        image.name()
                    ))
                })?;
            let hash = hex::encode(Sha256::digest(&bytes));
            let key = Key::variant(image.prefix(), &hash, kind.extension());
            state
                .storage
                .put_bytes(&key, bytes.into())
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            key.as_str().to_owned()
        }
        (None, false) => return Err(AppError::BadRequest("Choose an image first".into())),
    };
    let db = state.db.primary();
    let before = image.stored(&state.site.get().settings).to_owned();
    if before != value {
        let mut tx = db.begin().await?;
        moekura_db::settings::set_in(&mut tx, image.key(), json!(value))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(
                page.current.user.as_ref().map(|u| u.id),
                ActionKind::SettingUpdate,
            )
            .details(json!({ "key": image.key(), "from": before, "value": value })),
        )
        .await?;
        tx.commit().await?;
        state.site.reload(db).await?;
    }
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/admin/settings"),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(
            test_state(pool).await,
            super::routes()
                .merge(crate::admin::routes())
                .merge(crate::posts::routes()),
        )
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn rules_page_and_footer(pool: PgPool) {
        let app = app(&pool).await;
        assert_eq!(app.get("/rules", None).await.status, StatusCode::NOT_FOUND);
        let home = app.get("/", None).await.body;
        assert!(!home.contains("href=\"/rules\""), "{home}");

        let admin = session_for(&pool, "boss", SystemRole::Admin).await;
        let form = "site_name=Moekura&registration_mode=open&promotion_uploads=50\
                    &promotion_edits=0&promotion_account_days=30&promotion_max_recent_deletions=0\
                    &site_description=Pictures+of+cats\
                    &rules=No+%5B%5Bdogs%5D%5D.\
                    &footer_links=Our+Discord+https%3A%2F%2Fdiscord.gg%2Fabc%0D%0AHelp+%2Fwiki%2Fhelp";
        let saved = app
            .post_form("/admin/settings", Some(&admin), &[], form)
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);

        let rules = app.get("/rules", None).await;
        assert_eq!(rules.status, StatusCode::OK);
        assert!(rules.body.contains("href=\"/wiki/dogs\""), "{}", rules.body);
        let home = app.get("/", None).await.body;
        assert!(home.contains("<a href=\"/rules\">Rules</a>"), "{home}");
        assert!(
            home.contains("discord.gg&#x2f;abc\" rel=\"noopener\">Our Discord</a>"),
            "{home}"
        );
        assert!(home.contains("&#x2f;wiki&#x2f;help\">Help</a>"), "{home}");
        assert!(
            home.contains("<meta name=\"description\" content=\"Pictures of cats\">"),
            "{home}"
        );

        let bad = "site_name=Moekura&registration_mode=open&promotion_uploads=50\
                   &promotion_edits=0&promotion_account_days=30&promotion_max_recent_deletions=0\
                   &footer_links=Evil+javascript%3Aalert(1)";
        let refused = app
            .post_form("/admin/settings", Some(&admin), &[], bad)
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn logo_upload(pool: PgPool) {
        let app = app(&pool).await;
        let admin = session_for(&pool, "boss", SystemRole::Admin).await;
        let member = session_for(&pool, "pleb", SystemRole::Member).await;
        let png = fixture::png(64, 32);
        let file = Some(("logo.png", png.as_slice()));
        let forbidden = app
            .post_multipart_as("/admin/settings/logo", Some(&member), &[], "logo", file)
            .await;
        assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
        let not_image = app
            .post_multipart_as(
                "/admin/settings/logo",
                Some(&admin),
                &[],
                "logo",
                Some(("logo.png", b"<svg/>".as_slice())),
            )
            .await;
        assert_eq!(not_image.status, StatusCode::UNPROCESSABLE_ENTITY);

        let saved = app
            .post_multipart_as("/admin/settings/logo", Some(&admin), &[], "logo", file)
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        let home = app.get("/", None).await.body;
        let start = home.find("<img src=\"/data/logo/").expect(&home) + 10;
        let end = start + home[start..].find('"').unwrap();
        let logo = app.get_full(&home[start..end]).await;
        assert_eq!(logo.status(), StatusCode::OK);
        assert_eq!(logo.headers()["content-type"], "image/png");

        let removed = app
            .post_multipart(
                "/admin/settings/logo",
                Some(&admin),
                &[("remove", "1".into())],
                None,
            )
            .await;
        assert_eq!(removed.status, StatusCode::SEE_OTHER);
        assert!(!app.get("/", None).await.body.contains("/data/logo/"));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn full_logo_favicon_and_header_options(pool: PgPool) {
        let app = app(&pool).await;
        let admin = session_for(&pool, "boss", SystemRole::Admin).await;
        let png = fixture::png(64, 64);
        for (path, field, prefix) in [
            ("/admin/settings/full-logo", "full_logo", "/data/full-logo/"),
            ("/admin/settings/favicon", "favicon", "/data/favicon/"),
        ] {
            let saved = app
                .post_multipart_as(
                    path,
                    Some(&admin),
                    &[],
                    field,
                    Some(("a.png", png.as_slice())),
                )
                .await;
            assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
            let home = app.get("/", None).await.body;
            assert!(home.contains(prefix), "{home}");
        }
        let home = app.get("/", None).await.body;
        // The full logo stands for the icon and the name.
        assert!(home.contains("class=\"brand full-logo\""), "{home}");
        assert!(home.contains("alt=\"Moekura\""), "{home}");
        assert!(
            home.contains("<link rel=\"icon\" href=\"/data/favicon/"),
            "{home}"
        );

        let removed = app
            .post_multipart(
                "/admin/settings/full-logo",
                Some(&admin),
                &[("remove", "1".into())],
                None,
            )
            .await;
        assert_eq!(removed.status, StatusCode::SEE_OTHER);
        let form = "site_name=Moekura&registration_mode=open&promotion_uploads=50\
                    &promotion_edits=0&promotion_account_days=30&promotion_max_recent_deletions=0\
                    &hide_header_icon=on&hide_header_name=on";
        let saved = app
            .post_form("/admin/settings", Some(&admin), &[], form)
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        let home = app.get("/", None).await.body;
        assert!(!home.contains("full-logo"), "{home}");
        assert!(
            home.contains("<a class=\"brand\" href=\"/\" aria-label=\"Moekura\"></a>"),
            "{home}"
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn site_map_shows_what_each_may_use(pool: PgPool) {
        let app = app(&pool).await;
        // Links come from variables, so their slashes are escaped.
        let links_to = |body: &str, path: &str| {
            body.contains(&format!("href=\"{}\"", path.replace('/', "&#x2f;")))
        };
        let visitor = app.get("/site_map", None).await;
        assert_eq!(visitor.status, StatusCode::OK);
        assert!(links_to(&visitor.body, "/tags/aliases"), "{}", visitor.body);
        assert!(!links_to(&visitor.body, "/settings/api-keys"));
        assert!(!links_to(&visitor.body, "/admin/roles"));

        let admin = session_for(&pool, "boss", SystemRole::Admin).await;
        let page = app.get("/site_map", Some(&admin)).await.body;
        for link in [
            "/users/boss",
            "/settings/api-keys",
            "/moderation/queue",
            "/admin/roles",
        ] {
            assert!(links_to(&page, link), "{link}: {page}");
        }
    }
}
