//! User profiles and the logged-in user's settings.

use axum::extract::Path;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use serde::Deserialize;
use uwuu_core::permissions::Permission;
use uwuu_core::user_settings::{PER_PAGE_CHOICES, Theme, UserSettings};
use uwuu_db::users::{self, UserStatus};
use uwuu_db::{favorites, posts};

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::search_url;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users/{name}", get(profile))
        .route("/settings", get(settings_form).post(save_settings))
}

async fn profile(page: Page, Path(name): Path<String>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().db.read();
    let user = users::by_name(db, &name)
        .await?
        .filter(|u| u.status == UserStatus::Active || page.current.can(Permission::ManageUsers))
        .ok_or(AppError::NotFound)?;
    let site = page.state().site.get();
    let role = site.role(user.role_id).map(|r| r.name.clone());
    let uploads = posts::count_by_uploader(db, user.id).await?;
    let favorites = favorites::count_by_user(db, user.id).await?;
    Ok(page.render(
        "profile.html",
        context! {
            user => context! {
                name => user.name,
                role => role,
                joined => user.created_at.date().to_string(),
                status => format!("{:?}", user.status).to_lowercase(),
            },
            uploads => uploads,
            uploads_url => Value::from_safe_string(search_url(&format!("user:{}", user.name))),
            favorites => favorites,
            favorites_url => Value::from_safe_string(search_url(&format!("ordfav:{}", user.name))),
        },
    ))
}

async fn settings_form(page: Page) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let settings = UserSettings::from_json(&user.settings);
    let max = page.state().config.search.max_per_page;
    Ok(page.render(
        "settings.html",
        context! {
            per_page => settings.per_page,
            default_per_page => page.state().config.search.per_page,
            per_page_choices => PER_PAGE_CHOICES.iter().filter(|&&n| n <= max).collect::<Vec<_>>(),
            theme => settings.theme.as_str(),
            themes => Theme::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        },
    ))
}

#[derive(Debug, Deserialize)]
struct SettingsForm {
    /// Empty for the site default.
    #[serde(default)]
    per_page: String,
    #[serde(default)]
    theme: String,
}

async fn save_settings(
    page: Page,
    jar: CookieJar,
    Form(form): Form<SettingsForm>,
) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let max = page.state().config.search.max_per_page;
    let per_page = match form.per_page.as_str() {
        "" => None,
        n => Some(
            n.parse::<u32>()
                .ok()
                .filter(|n| PER_PAGE_CHOICES.contains(n) && *n <= max)
                .ok_or_else(|| AppError::BadRequest("Unknown page size".into()))?,
        ),
    };
    let theme =
        Theme::parse(&form.theme).ok_or_else(|| AppError::BadRequest("Unknown theme".into()))?;
    let settings = UserSettings { per_page, theme };
    users::set_settings(
        page.state().db.primary(),
        user.id,
        &settings.to_json(&user.settings),
    )
    .await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to("/settings")).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use sqlx::PgPool;
    use uwuu_core::permissions::SystemRole;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn profiles_and_settings(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::posts::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;

        let profile = app.get("/users/ALICE", None).await;
        assert_eq!(profile.status, StatusCode::OK);
        assert!(profile.body.contains("Member"), "{}", profile.body);
        assert!(profile.body.contains("href=\"/posts?tags=user%3Aalice\""));
        assert!(profile.body.contains("href=\"/posts?tags=ordfav%3Aalice\""));
        assert_eq!(
            app.get("/users/nobody", None).await.status,
            StatusCode::NOT_FOUND
        );

        // Visitors are sent to log in first.
        let visitor = app.get("/settings", None).await;
        assert_eq!(visitor.location.as_deref(), Some("/login?next=%2Fsettings"));
        let response = app
            .post_form("/settings", Some(&alice), &[], "per_page=100&theme=dark")
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        let page = app.get("/settings", Some(&alice)).await;
        assert!(page.body.contains("data-theme=\"dark\""), "{}", page.body);
        assert!(
            page.body.contains("value=\"100\" selected"),
            "{}",
            page.body
        );
        // The page size applies to searches.
        let grid = app.get("/", Some(&alice)).await;
        assert_eq!(grid.status, StatusCode::OK);

        let bad = app
            .post_form("/settings", Some(&alice), &[], "per_page=7&theme=dark")
            .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    }
}
