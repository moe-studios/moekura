//! User profiles and the logged-in user's settings.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::blacklist::Blacklist;
use moekura_core::permissions::Permission;
use moekura_core::user_settings::{PER_PAGE_CHOICES, Theme, UserSettings};
use moekura_db::users::{self, UserStatus};
use moekura_db::{favorites, posts};
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::search_url;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users/{name}", get(profile))
        .route(
            "/users/{name}/auto-promotion",
            axum::routing::post(set_auto_promotion),
        )
        .route("/settings", get(settings_form).post(save_settings))
}

async fn profile(page: Page, Path(name): Path<String>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let user = users::by_name(db, &name)
        .await?
        .filter(|u| u.status == UserStatus::Active || page.current.can(Permission::ManageUsers))
        .ok_or(AppError::NotFound)?;
    let site = page.state().site.get();
    let role = site.role(user.role_id).map(|r| r.name.clone());
    let uploads = posts::count_by_uploader(db, user.id).await?;
    let staff =
        page.current.can(Permission::BanUsers) || page.current.can(Permission::ViewAuditLog);
    let ban_history = if staff {
        moekura_db::bans::for_user(db, user.id).await?
    } else {
        Vec::new()
    };
    let can_ban = crate::bans::may_ban(page.state(), &page.current, &user);
    let banned = ban_history.iter().any(|b| b.active);
    let favorites = favorites::count_by_user(db, user.id).await?;
    let comments = moekura_db::comments::count_by_user(db, user.id).await?;
    let (promotion_blocked, promoted_at) = moekura_db::promotion::status(db, user.id).await?;
    let can_manage = page.current.can(Permission::ManageUsers)
        && page
            .state()
            .site
            .get()
            .role(user.role_id)
            .is_some_and(|r| page.current.role.outranks(r));
    let own = page.current.user.as_ref().map(|u| u.id) == Some(user.id);
    let favorite_groups = moekura_db::favorite_groups::for_user(db, user.id, own)
        .await?
        .len();
    let comments_url = format!(
        "/comments?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("user", &user.name)
            .finish()
    );
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
            comments => comments,
            promotion => context! {
                promoted => promoted_at.map(|at| at.date().to_string()),
                blocked => promotion_blocked,
                can_change => can_manage,
            },
            favorite_groups => favorite_groups,
            favorite_groups_url => crate::templates::url_value(&format!(
                "/favorite_groups?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("user", &user.name)
                    .finish()
            )),
            comments_url => crate::templates::url_value(&comments_url),
            bans => ban_history.iter().map(crate::bans::ban_context).collect::<Vec<_>>(),
            can_ban => can_ban && !banned,
            can_unban => can_ban && banned,
            durations => crate::bans::durations(),
        },
    ))
}

#[derive(Debug, Deserialize)]
struct AutoPromotionForm {
    /// Present to keep the user from automatic promotion.
    blocked: Option<String>,
}

/// Keeps a user from automatic promotion, or allows it again.
async fn set_auto_promotion(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(form): Form<AutoPromotionForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageUsers)?;
    let db = page.state().db.primary();
    let user = users::by_name(db, &name).await?.ok_or(AppError::NotFound)?;
    let outranks = page
        .state()
        .site
        .get()
        .role(user.role_id)
        .is_some_and(|r| page.current.role.outranks(r));
    if !outranks {
        return Err(AppError::Forbidden);
    }
    let blocked = form.blocked.is_some();
    moekura_db::promotion::set_blocked(db, user.id, blocked).await?;
    moekura_db::mod_actions::record(
        db,
        moekura_db::mod_actions::NewAction::new(
            page.current.user.as_ref().map(|u| u.id),
            moekura_core::moderation::ActionKind::UserStatus,
        )
        .user(user.id)
        .details(serde_json::json!({ "auto_promotion_blocked": blocked })),
    )
    .await?;
    let back = format!(
        "/users/{}",
        url::form_urlencoded::byte_serialize(user.name.as_bytes()).collect::<String>()
    );
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&back)).into_response())
}

async fn settings_form(page: Page) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let settings = UserSettings::from_json(&user.settings);
    let blacklist = crate::blacklist::text_for(page.state(), &page.current);
    let has_feed_token = moekura_db::feeds::has_token(page.state().db.primary(), user.id).await?;
    Ok(render_settings(
        &page,
        &settings,
        &blacklist,
        has_feed_token,
        None,
    ))
}

fn render_settings(
    page: &Page,
    settings: &UserSettings,
    blacklist: &str,
    has_feed_token: bool,
    error: Option<String>,
) -> Response {
    let max = page.state().config.search.max_per_page;
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    page.render_with_status(
        status,
        "settings.html",
        context! {
            error => error,
            blacklist => blacklist,
            has_feed_token => has_feed_token,
            per_page => settings.per_page,
            default_per_page => page.state().config.search.per_page,
            per_page_choices => PER_PAGE_CHOICES.iter().filter(|&&n| n <= max).collect::<Vec<_>>(),
            current_theme => settings.theme.as_str(),
            themes => Theme::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        },
    )
}

#[derive(Debug, Deserialize)]
struct SettingsForm {
    /// Empty for the site default.
    #[serde(default)]
    per_page: String,
    #[serde(default)]
    theme: String,
    #[serde(default)]
    blacklist: String,
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
    let blacklist = form.blacklist.replace("\r\n", "\n");
    let settings = UserSettings {
        per_page,
        theme,
        blacklist: Some(blacklist.trim().to_owned()),
    };
    if let Err(error) = Blacklist::parse(&blacklist) {
        let has_feed_token =
            moekura_db::feeds::has_token(page.state().db.primary(), user.id).await?;
        return Ok(render_settings(
            &page,
            &settings,
            &blacklist,
            has_feed_token,
            Some(error.to_string()),
        ));
    }
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
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn profiles_and_settings(pool: PgPool) {
        moekura_db::settings::set(&pool, "default_blacklist", serde_json::json!("rating:e"))
            .await
            .unwrap();
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
        assert!(profile.body.contains("href=\"/comments?user=alice\""));
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

        // The blacklist starts as the site default.
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let page = app.get("/settings", Some(&bob)).await;
        assert!(page.body.contains(">rating:e</textarea>"), "{}", page.body);
        let response = app
            .post_form(
                "/settings",
                Some(&bob),
                &[],
                "theme=system&blacklist=score%3A1",
            )
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            response.body.contains("only tags, -tags and rating:"),
            "{}",
            response.body
        );

        let bad = app
            .post_form("/settings", Some(&alice), &[], "per_page=7&theme=dark")
            .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn staff_keep_users_from_promotion(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        session_for(&pool, "alice", SystemRole::Member).await;
        let member = session_for(&pool, "bob", SystemRole::Member).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        assert!(
            !app.get("/users/alice", Some(&member))
                .await
                .body
                .contains("auto-promotion")
        );
        let page = app.get("/users/alice", Some(&admin)).await;
        assert!(
            page.body.contains("Never promote automatically"),
            "{}",
            page.body
        );
        assert_eq!(
            app.post_form(
                "/users/alice/auto-promotion",
                Some(&member),
                &[],
                "blocked=1"
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        let saved = app
            .post_form(
                "/users/alice/auto-promotion",
                Some(&admin),
                &[],
                "blocked=1",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        assert!(
            app.get("/users/alice", Some(&admin))
                .await
                .body
                .contains("Kept from automatic promotion.")
        );
        // Not someone of the same rank.
        session_for(&pool, "root2", SystemRole::Admin).await;
        assert_eq!(
            app.post_form("/users/root2/auto-promotion", Some(&admin), &[], "")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
    }
}
