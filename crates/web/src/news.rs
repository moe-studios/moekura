//! Site news: admins post short announcements at `/admin/news`, shown at
//! the top of every page until they expire or each reader dismisses them
//! (in their settings when logged in, and in a cookie either way).

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::news::{self, NewsUpdate};
use moekura_db::site_cache::SiteSnapshot;
use moekura_db::users::User;
use serde::Deserialize;
use time::{Date, OffsetDateTime, Time};

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

/// Holds the newest news this browser dismissed.
const COOKIE: &str = "news_dismissed";
/// The longest news, in characters.
const MAX_LEN: usize = 2000;
/// News per page of the admin list.
const PAGE_SIZE: i64 = 50;
const DATE_FORMAT: &[time::format_description::BorrowedFormatItem<'_>] =
    time::macros::format_description!("[year]-[month]-[day]");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/news_updates/{id}/dismiss", post(dismiss))
        .route("/admin/news", get(index).post(create))
        .route("/admin/news/{id}", get(edit_form).post(update))
        .route("/admin/news/{id}/{action}", post(set_deleted))
}

/// The newest news the browser's cookie says was dismissed.
pub(crate) fn dismissed(jar: &CookieJar) -> Option<i64> {
    jar.get(COOKIE).and_then(|c| c.value().parse().ok())
}

/// The news banner for the layout, unless there's none or the reader
/// dismissed it (by `cookie`, or in their settings).
pub(crate) fn banner(
    site: &SiteSnapshot,
    user: Option<&User>,
    cookie: Option<i64>,
) -> Option<Value> {
    let news = site.news()?;
    let in_settings = user.and_then(|u| u.settings.get("dismissed_news")?.as_i64());
    if cookie.max(in_settings).is_some_and(|seen| seen >= news.id) {
        return None;
    }
    Some(context! {
        id => news.id,
        html => Value::from_safe_string(markup::render(&news.body)),
    })
}

#[derive(Debug, Default, Deserialize)]
struct DismissForm {
    back: Option<String>,
}

async fn dismiss(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<DismissForm>,
) -> Result<Response, AppError> {
    if let Some(user) = &page.current.user {
        news::dismiss(page.state().db.primary(), user.id, id).await?;
    }
    let jar = jar.add(
        Cookie::build((COOKIE, id.to_string()))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax)
            .max_age(time::Duration::days(365))
            .build(),
    );
    Ok((
        jar,
        Redirect::to(crate::account::safe_next(form.back.as_deref())),
    )
        .into_response())
}

// ---- admin ----------------------------------------------------------------

/// The last day (UTC) news ending at `end` is shown: the one before.
fn last_day(end: OffsetDateTime) -> Option<String> {
    end.date().previous_day()?.format(DATE_FORMAT).ok()
}

fn news_context(n: &NewsUpdate) -> Value {
    let now = OffsetDateTime::now_utc();
    context! {
        id => n.id,
        html => Value::from_safe_string(markup::render(&n.body)),
        creator => n.creator_name,
        date => crate::dates::day(n.created_at),
        until => n.expires_at.and_then(last_day),
        state => if n.is_deleted {
            "deleted"
        } else if n.is_current(now) {
            "shown"
        } else {
            "expired"
        },
        deleted => n.is_deleted,
    }
}

#[derive(Debug, Default, Deserialize)]
struct NewsForm {
    #[serde(default)]
    body: String,
    /// The last day it's shown (`YYYY-MM-DD`, UTC); empty for no end.
    #[serde(default)]
    until: String,
}

/// The body and expiry `form` asks for, or why they won't do.
fn checked(form: &NewsForm) -> Result<(String, Option<OffsetDateTime>), String> {
    let body = form.body.replace("\r\n", "\n").trim().to_owned();
    if body.is_empty() {
        return Err("Say what the news is.".into());
    }
    if body.chars().count() > MAX_LEN {
        return Err(format!("News can be at most {MAX_LEN} characters long."));
    }
    let until = match form.until.trim() {
        "" => None,
        text => {
            let day =
                Date::parse(text, DATE_FORMAT).map_err(|_| "That's not a date.".to_owned())?;
            // Shown through the whole of its last day.
            let end = day
                .next_day()
                .ok_or_else(|| "That's not a date.".to_owned())?
                .with_time(Time::MIDNIGHT)
                .assume_utc();
            if end <= OffsetDateTime::now_utc() {
                return Err("That day has passed.".into());
            }
            Some(end)
        }
    };
    Ok((body, until))
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    page: Option<i64>,
}

async fn render_index(
    page: &Page,
    number: i64,
    form: &NewsForm,
    error: Option<String>,
) -> Result<Response, AppError> {
    let mut found = news::list(
        page.state().db.primary(),
        true,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let more = found.len() > PAGE_SIZE as usize;
    found.truncate(PAGE_SIZE as usize);
    let link = |n: i64| format!("/admin/news?page={n}");
    Ok(page.render_with_status(
        if error.is_some() {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::OK
        },
        "admin_news.html",
        context! {
            news => found.iter().map(news_context).collect::<Vec<_>>(),
            form => context! { body => form.body, until => form.until },
            error => error,
            max_len => MAX_LEN,
            previous_url => (number > 1).then(|| link(number - 1)),
            next_url => more.then(|| link(number + 1)),
        },
    ))
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let number = query.page.unwrap_or(1).clamp(1, 1000);
    render_index(&page, number, &NewsForm::default(), None).await
}

fn actor(page: &Page) -> Option<i64> {
    page.current.user.as_ref().map(|u| u.id)
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<NewsForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let (body, until) = match checked(&form) {
        Ok(checked) => checked,
        Err(message) => return render_index(&page, 1, &form, Some(message)).await,
    };
    let state = page.state();
    let mut tx = state.db.primary().begin().await?;
    let id = news::create(&mut tx, actor(&page), &body, until).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor(&page), ActionKind::NewsPost)
            .details(serde_json::json!({ "news_id": id, "body": body })),
    )
    .await?;
    tx.commit().await?;
    state.site.reload(state.db.primary()).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to("/admin/news")).into_response())
}

fn render_edit(page: &Page, news: &NewsUpdate, form: &NewsForm, error: Option<String>) -> Response {
    page.render_with_status(
        if error.is_some() {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::OK
        },
        "admin_news_edit.html",
        context! {
            id => news.id,
            form => context! { body => form.body, until => form.until },
            error => error,
            max_len => MAX_LEN,
        },
    )
}

async fn found(page: &Page, id: i64) -> Result<NewsUpdate, AppError> {
    news::by_id(page.state().db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)
}

async fn edit_form(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let news = found(&page, id).await?;
    let form = NewsForm {
        body: news.body.clone(),
        until: news.expires_at.and_then(last_day).unwrap_or_default(),
    };
    Ok(render_edit(&page, &news, &form, None))
}

async fn update(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<NewsForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let news = found(&page, id).await?;
    let (body, until) = match checked(&form) {
        Ok(checked) => checked,
        Err(message) => return Ok(render_edit(&page, &news, &form, Some(message))),
    };
    let state = page.state();
    let mut tx = state.db.primary().begin().await?;
    news::update(&mut tx, id, &body, until).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor(&page), ActionKind::NewsPost)
            .details(serde_json::json!({ "news_id": id, "body": body })),
    )
    .await?;
    tx.commit().await?;
    state.site.reload(state.db.primary()).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to("/admin/news")).into_response())
}

async fn set_deleted(
    page: Page,
    jar: CookieJar,
    Path((id, action)): Path<(i64, String)>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let deleted = match action.as_str() {
        "delete" => true,
        "restore" => false,
        _ => return Err(AppError::NotFound),
    };
    let news = found(&page, id).await?;
    let state = page.state();
    let mut tx = state.db.primary().begin().await?;
    if news::set_deleted(&mut tx, id, deleted).await? {
        mod_actions::record(
            &mut *tx,
            NewAction::new(
                actor(&page),
                if deleted {
                    ActionKind::NewsDelete
                } else {
                    ActionKind::NewsPost
                },
            )
            .details(serde_json::json!({ "news_id": id, "body": news.body })),
        )
        .await?;
    }
    tx.commit().await?;
    state.site.reload(state.db.primary()).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to("/admin/news")).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn site_news(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes()
                .merge(crate::users::routes())
                .merge(crate::danbooru::test_support::routes()),
        );
        let admin = session_for(&pool, "admin", SystemRole::Admin).await;
        let member = session_for(&pool, "member", SystemRole::Member).await;

        assert_eq!(
            app.get("/admin/news", Some(&member)).await.status,
            StatusCode::FORBIDDEN
        );
        let passed = app
            .post_form(
                "/admin/news",
                Some(&admin),
                &[],
                "body=Late&until=2000-01-01",
            )
            .await;
        assert_eq!(passed.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(passed.body.contains("That day has passed."));
        let posted = app
            .post_form(
                "/admin/news",
                Some(&admin),
                &[],
                "body=We+moved+to+%5Bb%5Dnew+servers%5B%2Fb%5D.",
            )
            .await;
        assert_eq!(posted.location.as_deref(), Some("/admin/news"));

        // Everyone sees it until they dismiss it.
        for session in [None, Some(member.as_str())] {
            let home = app.get("/users/admin", session).await.body;
            assert!(home.contains("class=\"news\""), "{home}");
            assert!(home.contains("<strong>new servers</strong>"), "{home}");
        }
        let id: i64 = sqlx::query_scalar("SELECT id FROM news_updates")
            .fetch_one(&pool)
            .await
            .unwrap();
        let dismissed = app
            .post_form(
                &format!("/news_updates/{id}/dismiss"),
                Some(&member),
                &[],
                "back=%2Fposts",
            )
            .await;
        assert_eq!(dismissed.location.as_deref(), Some("/posts"));
        // Remembered in their settings, without the cookie.
        assert!(
            !app.get("/users/admin", Some(&member))
                .await
                .body
                .contains("class=\"news\"")
        );
        // Visitors carry the cookie.
        let cookie = format!("news_dismissed={id}");
        assert!(
            !app.get_with_cookie("/users/admin", &cookie)
                .await
                .body
                .contains("class=\"news\"")
        );

        // Newer news shows again; deleted news doesn't.
        app.post_form("/admin/news", Some(&admin), &[], "body=Newer")
            .await;
        assert!(
            app.get("/users/admin", Some(&member))
                .await
                .body
                .contains("Newer")
        );
        let newer: i64 = sqlx::query_scalar("SELECT max(id) FROM news_updates")
            .fetch_one(&pool)
            .await
            .unwrap();
        app.post_form(
            &format!("/admin/news/{newer}/delete"),
            Some(&admin),
            &[],
            "",
        )
        .await;
        assert!(
            !app.get("/users/admin", Some(&member))
                .await
                .body
                .contains("class=\"news\"")
        );
        assert!(
            app.get("/users/admin", None)
                .await
                .body
                .contains("new servers")
        );

        // Editing.
        let edit = app
            .get(&format!("/admin/news/{id}"), Some(&admin))
            .await
            .body;
        assert!(edit.contains("[b]new servers[&#x2f;b]"), "{edit}");
        app.post_form(
            &format!("/admin/news/{id}"),
            Some(&admin),
            &[],
            "body=Moved.&until=2999-12-31",
        )
        .await;
        let list = app.get("/admin/news", Some(&admin)).await.body;
        assert!(list.contains("until 2999-12-31"), "{list}");
        let logged: Vec<String> = sqlx::query_scalar("SELECT action FROM mod_actions ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(
            logged,
            ["news.post", "news.post", "news.delete", "news.post"]
        );

        // Danbooru apps read it, deleted news left out.
        let found: Value =
            serde_json::from_str(&app.get("/news_updates.json", None).await.body).unwrap();
        assert_eq!(found.as_array().unwrap().len(), 1);
        assert_eq!(found[0]["message"], "Moved.");
    }
}
