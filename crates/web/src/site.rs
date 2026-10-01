//! The site's own pages and identity: the rules page and the logo.

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

/// The largest logo accepted.
const LOGO_MAX_BYTES: usize = 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new().route("/rules", get(rules)).route(
        "/admin/settings/logo",
        post(set_logo).layer(DefaultBodyLimit::max(LOGO_MAX_BYTES + 16 * 1024)),
    )
}

/// Where browsers load the logo from, if the site has one.
pub fn logo_url(state: &AppState) -> Option<Value> {
    let site = state.site.get();
    let key = Key::parse(&site.settings.logo)?;
    Some(crate::templates::url_value(&state.file_url(&key)))
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

/// Replaces the logo with an uploaded image, or removes it (`remove`).
async fn set_logo(page: Page, jar: CookieJar, mut form: Multipart) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let mut upload: Option<Vec<u8>> = None;
    let mut remove = false;
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("The logo must be at most 1 MB".into()))?
    {
        match field.name() {
            Some("logo") => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|_| AppError::BadRequest("The logo must be at most 1 MB".into()))?;
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
                    AppError::Unprocessable(
                        "The logo must be a PNG, JPEG, GIF, WebP or AVIF image".into(),
                    )
                })?;
            let hash = hex::encode(Sha256::digest(&bytes));
            let key = Key::variant("logo", &hash, kind.extension());
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
    let before = state.site.get().settings.logo.clone();
    if before != value {
        let mut tx = db.begin().await?;
        moekura_db::settings::set_in(&mut tx, "logo", json!(value))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(
                page.current.user.as_ref().map(|u| u.id),
                ActionKind::SettingUpdate,
            )
            .details(json!({ "key": "logo", "from": before, "value": value })),
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
}
