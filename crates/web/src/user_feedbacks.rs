//! Feedback on users: staff and senior users (with *Leave feedback*) note
//! positive, neutral or negative things about someone of lower rank. It's
//! public, shown on profiles and weighed in automatic promotion; its
//! writer can change it, and staff who ban users can delete it if they
//! outrank whom it's on, and who wrote it unless it's theirs, and restore
//! it if they also outrank who deleted it, unless they did.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::user_feedbacks::{self, CATEGORIES, Feedback, Filter};
use moekura_db::users::{self, User};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::url_value;

/// Feedback per page.
const PAGE_SIZE: i64 = 50;
/// The longest feedback, in characters.
const MAX_LEN: usize = 20_000;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/user_feedbacks", get(index).post(create))
        .route("/user_feedbacks/new", get(new_form))
        .route("/user_feedbacks/{id}", post(update))
        .route("/user_feedbacks/{id}/edit", get(edit_form))
        .route("/user_feedbacks/{id}/{action}", post(moderate))
}

/// Whether `current` may leave feedback on `user`: with the permission,
/// on someone else of lower rank.
pub(crate) fn may_give(state: &AppState, current: &CurrentUser, user: &User) -> bool {
    current.can(Permission::GiveFeedback)
        && current.user.as_ref().is_some_and(|me| me.id != user.id)
        && state
            .site
            .get()
            .role(user.role_id)
            .is_some_and(|role| current.role.outranks(role))
}

/// Whether `current` may delete or restore feedback `f`: staff who ban
/// users, on someone else ranked below them, written by them or by
/// someone else ranked below them, and if it's deleted, deleted by them
/// or by someone ranked below them, so nobody undoes what those above
/// them decided.
pub(crate) fn may_moderate(state: &AppState, current: &CurrentUser, f: &Feedback) -> bool {
    let site = state.site.get();
    let below = |role_id: i32| {
        site.role(role_id)
            .is_some_and(|role| current.role.outranks(role))
    };
    current.can(Permission::BanUsers)
        && current.user.as_ref().is_some_and(|me| {
            let theirs_or_below = |id: Option<i64>, role_id: Option<i32>| {
                id == Some(me.id) || role_id.is_none_or(below)
            };
            me.id != f.user_id
                && below(f.user_role_id)
                && theirs_or_below(f.creator_id, f.creator_role_id)
                && (!f.is_deleted || theirs_or_below(f.deleted_by_id, f.deleted_by_role_id))
        })
}

/// Where `name`'s feedback is listed.
pub(crate) fn list_url(name: &str) -> String {
    format!(
        "/user_feedbacks?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("user", name)
            .finish()
    )
}

fn clean(category: &str, body: &str) -> Result<(String, String), AppError> {
    let category = category.trim();
    if !CATEGORIES.contains(&category) {
        return Err(AppError::Unprocessable(
            "Choose positive, neutral or negative.".into(),
        ));
    }
    let body = body.replace("\r\n", "\n").trim_end().to_owned();
    if body.trim().is_empty() {
        return Err(AppError::Unprocessable("Say what the feedback is.".into()));
    }
    if body.chars().count() > MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "Feedback can be at most {MAX_LEN} characters long."
        )));
    }
    Ok((category.to_owned(), body))
}

fn feedback_context(state: &AppState, f: &Feedback, current: &CurrentUser) -> Value {
    let mine = f.creator_id.is_some() && f.creator_id == current.user.as_ref().map(|u| u.id);
    context! {
        id => f.id,
        user => f.user_name,
        creator => f.creator_name,
        category => f.category,
        html => Value::from_safe_string(markup::render(&f.body)),
        deleted => f.is_deleted,
        edited => f.updated_at > f.created_at,
        date => crate::dates::day(f.created_at),
        can_edit => mine && !f.is_deleted && current.can(Permission::GiveFeedback),
        can_delete => may_moderate(state, current, f),
    }
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    #[serde(default)]
    user: String,
    #[serde(default)]
    creator: String,
    #[serde(default)]
    category: String,
    page: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let db = state.reader(&page.current);
    let named = async |name: &str| -> Result<Option<User>, AppError> {
        if name.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(
            users::by_name(db, name.trim())
                .await?
                .ok_or(AppError::NotFound)?,
        ))
    };
    let user = named(&query.user).await?;
    let creator = named(&query.creator).await?;
    let category = CATEGORIES.into_iter().find(|c| *c == query.category.trim());
    let number = query.page.unwrap_or(1).clamp(1, 1000);
    let filter = Filter {
        user_id: user.as_ref().map(|u| u.id),
        creator_id: creator.as_ref().map(|u| u.id),
        category,
        with_deleted: page.current.can(Permission::BanUsers),
    };
    let mut found =
        user_feedbacks::list(db, &filter, (number - 1) * PAGE_SIZE, PAGE_SIZE + 1).await?;
    let more = found.len() > PAGE_SIZE as usize;
    found.truncate(PAGE_SIZE as usize);
    let link = |category: Option<&str>, n: i64| {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if let Some(user) = &user {
            query.append_pair("user", &user.name);
        }
        if let Some(creator) = &creator {
            query.append_pair("creator", &creator.name);
        }
        if let Some(category) = category {
            query.append_pair("category", category);
        }
        if n > 1 {
            query.append_pair("page", &n.to_string());
        }
        url_value(&format!("/user_feedbacks?{}", query.finish()))
    };
    let new_url = user
        .as_ref()
        .filter(|u| may_give(state, &page.current, u))
        .map(|u| {
            url_value(&format!(
                "/user_feedbacks/new?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("user", &u.name)
                    .finish()
            ))
        });
    Ok(page.render(
        "user_feedbacks.html",
        context! {
            user => user.as_ref().map(|u| &u.name),
            creator => creator.as_ref().map(|u| &u.name),
            category => category,
            categories => std::iter::once(context! { label => "all", url => link(None, 1), current => category.is_none() })
                .chain(CATEGORIES.iter().map(|c| context! {
                    label => c,
                    url => link(Some(c), 1),
                    current => category == Some(*c),
                }))
                .collect::<Vec<_>>(),
            feedbacks => found.iter().map(|f| feedback_context(state, f, &page.current)).collect::<Vec<_>>(),
            new_url => new_url,
            previous_url => (number > 1).then(|| link(category, number - 1)),
            next_url => more.then(|| link(category, number + 1)),
        },
    ))
}

fn form_context(
    user: &str,
    action: &str,
    category: &str,
    body: &str,
    error: Option<String>,
) -> Value {
    context! {
        user => user,
        user_url => url_value(&format!("/users/{}", urlencoding(user))),
        action => url_value(action),
        category => category,
        categories => CATEGORIES,
        body => body,
        error => error,
    }
}

fn urlencoding(name: &str) -> String {
    url::form_urlencoded::byte_serialize(name.as_bytes()).collect()
}

#[derive(Debug, Default, Deserialize)]
struct NewQuery {
    #[serde(default)]
    user: String,
}

/// The user feedback is being left on, if `current` may.
async fn subject(state: &AppState, current: &CurrentUser, name: &str) -> Result<User, AppError> {
    current.require(Permission::GiveFeedback)?;
    let user = users::by_name(state.db.primary(), name.trim())
        .await?
        .ok_or_else(|| {
            AppError::Unprocessable(format!("There's no user called “{}”.", name.trim()))
        })?;
    if !may_give(state, current, &user) {
        return Err(AppError::Forbidden);
    }
    Ok(user)
}

async fn new_form(page: Page, Query(query): Query<NewQuery>) -> Result<Response, AppError> {
    let user = subject(page.state(), &page.current, &query.user).await?;
    Ok(page.render(
        "user_feedback_form.html",
        form_context(&user.name, "/user_feedbacks", "neutral", "", None),
    ))
}

#[derive(Debug, Default, Deserialize)]
struct FeedbackForm {
    #[serde(default)]
    user: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    body: String,
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<FeedbackForm>,
) -> Result<Response, AppError> {
    let state = page.state();
    let user = subject(state, &page.current, &form.user).await?;
    let me = page
        .current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)?;
    let (category, body) = match clean(&form.category, &form.body) {
        Ok(cleaned) => cleaned,
        Err(AppError::Unprocessable(message)) => {
            return Ok(page.render_with_status(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "user_feedback_form.html",
                form_context(
                    &user.name,
                    "/user_feedbacks",
                    &form.category,
                    &form.body,
                    Some(message),
                ),
            ));
        }
        Err(error) => return Err(error),
    };
    state.rate_limits.check_comment(me).await?;
    user_feedbacks::create(state.db.primary(), user.id, me, &category, &body).await?;
    let url = list_url(&user.name);
    crate::notifications::notify(
        state,
        &[user.id],
        moekura_db::notifications::Kind::Feedback,
        Some(me),
        &format!("{category} feedback on your account"),
        &url,
    )
    .await;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&url)).into_response())
}

/// Feedback `id`, if `current` wrote it and may still change it.
async fn own(state: &AppState, current: &CurrentUser, id: i64) -> Result<Feedback, AppError> {
    current.require(Permission::GiveFeedback)?;
    let f = user_feedbacks::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    if f.creator_id.is_none() || f.creator_id != current.user.as_ref().map(|u| u.id) || f.is_deleted
    {
        return Err(AppError::Forbidden);
    }
    Ok(f)
}

async fn edit_form(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let f = own(page.state(), &page.current, id).await?;
    Ok(page.render(
        "user_feedback_form.html",
        form_context(
            &f.user_name,
            &format!("/user_feedbacks/{id}"),
            &f.category,
            &f.body,
            None,
        ),
    ))
}

async fn update(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<FeedbackForm>,
) -> Result<Response, AppError> {
    let state = page.state();
    let f = own(state, &page.current, id).await?;
    let (category, body) = match clean(&form.category, &form.body) {
        Ok(cleaned) => cleaned,
        Err(AppError::Unprocessable(message)) => {
            return Ok(page.render_with_status(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "user_feedback_form.html",
                form_context(
                    &f.user_name,
                    &format!("/user_feedbacks/{id}"),
                    &form.category,
                    &form.body,
                    Some(message),
                ),
            ));
        }
        Err(error) => return Err(error),
    };
    user_feedbacks::update(state.db.primary(), id, &category, &body).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&list_url(&f.user_name)),
    )
        .into_response())
}

/// Deleting and restoring, by staff who ban users, on feedback about and
/// by those ranked below them, and restoring only what they or those
/// below them deleted; logged.
async fn moderate(
    page: Page,
    jar: CookieJar,
    Path((id, action)): Path<(i64, String)>,
) -> Result<Response, AppError> {
    page.current.require(Permission::BanUsers)?;
    let deleted = match action.as_str() {
        "delete" => true,
        "restore" => false,
        _ => return Err(AppError::NotFound),
    };
    let actor = page.current.user.as_ref().map(|u| u.id);
    // Locked, so that who deleted it can't change before it's restored.
    let mut tx = page.state().db.primary().begin().await?;
    let f = user_feedbacks::lock(&mut *tx, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !may_moderate(page.state(), &page.current, &f) {
        return Err(AppError::Forbidden);
    }
    if user_feedbacks::set_deleted(&mut *tx, id, deleted, actor).await? {
        mod_actions::record(
            &mut *tx,
            NewAction::new(
                actor,
                if deleted {
                    ActionKind::FeedbackDelete
                } else {
                    ActionKind::FeedbackRestore
                },
            )
            .user(f.user_id)
            .details(serde_json::json!({ "feedback_id": id, "category": f.category })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&list_url(&f.user_name)),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn feedback_on_users(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes()
                .merge(crate::users::routes())
                .merge(crate::notifications::routes())
                .merge(crate::danbooru::test_support::routes()),
        );
        let member = session_for(&pool, "member", SystemRole::Member).await;
        let senior = session_for(&pool, "senior", SystemRole::Contributor).await;
        let staff = session_for(&pool, "staff", SystemRole::Moderator).await;

        // Members can't; contributors can, on those below them.
        assert_eq!(
            app.get("/user_feedbacks/new?user=other", Some(&member))
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.get("/user_feedbacks/new?user=staff", Some(&senior))
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let profile = app.get("/users/member", Some(&senior)).await.body;
        assert!(profile.contains("Leave feedback"), "{profile}");
        assert!(
            profile.contains("<dt>Feedback</dt><dd><a href=\"/user_feedbacks?user=member\">None")
        );

        let empty = app
            .post_form(
                "/user_feedbacks",
                Some(&senior),
                &[],
                "user=member&category=negative&body=",
            )
            .await;
        assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
        let made = app
            .post_form(
                "/user_feedbacks",
                Some(&senior),
                &[],
                "user=member&category=negative&body=Tags+badly",
            )
            .await;
        assert_eq!(
            made.location.as_deref(),
            Some("/user_feedbacks?user=member")
        );
        app.post_form(
            "/user_feedbacks",
            Some(&staff),
            &[],
            "user=member&category=positive&body=Better",
        )
        .await;
        let profile = app.get("/users/member", None).await.body;
        assert!(
            profile.contains(
                "1 positive</span>, 0 neutral, <span class=\"status-negative\">1 negative"
            ),
            "{profile}"
        );
        let listed = app
            .get("/user_feedbacks?user=member&category=negative", None)
            .await
            .body;
        assert!(
            listed.contains("Tags badly") && !listed.contains("Better"),
            "{listed}"
        );
        assert!(
            app.get("/notifications", Some(&member))
                .await
                .body
                .contains("senior left negative feedback on your account")
        );

        // Its writer edits it; nobody else does.
        let id: i64 =
            sqlx::query_scalar("SELECT id FROM user_feedbacks WHERE category = 'negative'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            app.get(&format!("/user_feedbacks/{id}/edit"), Some(&staff))
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        app.post_form(
            &format!("/user_feedbacks/{id}"),
            Some(&senior),
            &[],
            "category=neutral&body=Tags+oddly",
        )
        .await;

        // Danbooru clients read it.
        let found: Value = serde_json::from_str(
            &app.get(
                "/user_feedbacks.json?search[user_name]=member&search[category]=neutral",
                None,
            )
            .await
            .body,
        )
        .unwrap();
        assert_eq!(found[0]["body"], "Tags oddly");
        assert_eq!(found.as_array().unwrap().len(), 1);

        // Staff delete it, which only they then see.
        assert_eq!(
            app.post_form(
                &format!("/user_feedbacks/{id}/delete"),
                Some(&senior),
                &[],
                ""
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        app.post_form(
            &format!("/user_feedbacks/{id}/delete"),
            Some(&staff),
            &[],
            "",
        )
        .await;
        assert!(
            !app.get("/user_feedbacks?user=member", None)
                .await
                .body
                .contains("Tags oddly")
        );
        assert!(
            app.get("/user_feedbacks?user=member", Some(&staff))
                .await
                .body
                .contains("Tags oddly")
        );
        assert_eq!(
            app.get(&format!("/user_feedbacks/{id}.json"), None)
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        let logged: String = sqlx::query_scalar("SELECT action FROM mod_actions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(logged, "user_feedback.delete");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn deleting_feedback_respects_rank(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        session_for(&pool, "member", SystemRole::Member).await;
        let moderator = session_for(&pool, "moderator", SystemRole::Moderator).await;
        let other = session_for(&pool, "other", SystemRole::Moderator).await;
        let admin = session_for(&pool, "admin", SystemRole::Admin).await;
        let give = async |by: &str, on: &str, body: &str| -> i64 {
            let made = app
                .post_form(
                    "/user_feedbacks",
                    Some(by),
                    &[],
                    &format!("user={on}&category=negative&body={body}"),
                )
                .await;
            assert_eq!(made.status, StatusCode::SEE_OTHER, "{}", made.body);
            sqlx::query_scalar("SELECT id FROM user_feedbacks WHERE body = $1")
                .bind(body)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        let act = async |who: &str, id: i64, action: &str| {
            app.post_form(
                &format!("/user_feedbacks/{id}/{action}"),
                Some(who),
                &[],
                "",
            )
            .await
            .status
        };
        let on_moderator = give(&admin, "moderator", "Overreach").await;
        let by_admin = give(&admin, "member", "Spam").await;
        let by_moderator = give(&moderator, "member", "Rude").await;

        // Not on themselves, nor what someone above them wrote.
        assert_eq!(
            act(&moderator, on_moderator, "delete").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            act(&other, on_moderator, "delete").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            act(&moderator, by_admin, "delete").await,
            StatusCode::FORBIDDEN
        );
        let page = app
            .get("/user_feedbacks?user=member", Some(&moderator))
            .await;
        assert_eq!(page.body.matches("/delete\"").count(), 1, "{}", page.body);
        // Their own, or below them all; not a peer's.
        assert_eq!(
            act(&moderator, by_moderator, "delete").await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            act(&other, by_moderator, "restore").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            act(&admin, by_moderator, "restore").await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            act(&admin, on_moderator, "delete").await,
            StatusCode::SEE_OTHER
        );
        // Nor undo what someone above them did.
        assert_eq!(
            act(&moderator, on_moderator, "restore").await,
            StatusCode::FORBIDDEN
        );
        let deleted: Vec<String> =
            sqlx::query_scalar("SELECT body FROM user_feedbacks WHERE is_deleted ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(deleted, ["Overreach"]);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn restoring_feedback_respects_who_deleted_it(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        session_for(&pool, "member", SystemRole::Member).await;
        let contributor = session_for(&pool, "contributor", SystemRole::Contributor).await;
        let moderator = session_for(&pool, "moderator", SystemRole::Moderator).await;
        let other = session_for(&pool, "other", SystemRole::Moderator).await;
        let admin = session_for(&pool, "admin", SystemRole::Admin).await;
        let give = async |by: &str, body: &str| -> i64 {
            let made = app
                .post_form(
                    "/user_feedbacks",
                    Some(by),
                    &[],
                    &format!("user=member&category=negative&body={body}"),
                )
                .await;
            assert_eq!(made.status, StatusCode::SEE_OTHER, "{}", made.body);
            sqlx::query_scalar("SELECT id FROM user_feedbacks WHERE body = $1")
                .bind(body)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        let act = async |who: &str, id: i64, action: &str| {
            app.post_form(
                &format!("/user_feedbacks/{id}/{action}"),
                Some(who),
                &[],
                "",
            )
            .await
            .status
        };
        let restorable = async |who: &str| {
            app.get("/user_feedbacks?user=member", Some(who))
                .await
                .body
                .matches("/restore\"")
                .count()
        };
        let by_moderator = give(&moderator, "Rude").await;
        let by_contributor = give(&contributor, "Spam").await;

        // What an admin deleted stays deleted for those below them, even
        // when they wrote it.
        assert_eq!(
            act(&admin, by_moderator, "delete").await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            act(&admin, by_contributor, "delete").await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(restorable(&moderator).await, 0);
        assert_eq!(
            act(&moderator, by_moderator, "restore").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            act(&moderator, by_contributor, "restore").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(restorable(&admin).await, 2);

        // What they deleted themselves, they restore; a peer doesn't.
        assert_eq!(
            act(&admin, by_contributor, "restore").await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            act(&moderator, by_contributor, "delete").await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(restorable(&other).await, 0);
        assert_eq!(
            act(&other, by_contributor, "restore").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(restorable(&moderator).await, 1);
        assert_eq!(
            act(&moderator, by_contributor, "restore").await,
            StatusCode::SEE_OTHER
        );
        let deleted: Vec<(String, Option<i64>)> = sqlx::query_as(
            "SELECT body, deleted_by_id FROM user_feedbacks WHERE is_deleted OR deleted_by_id IS NOT NULL ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let admin_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'admin'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(deleted, [("Rude".to_owned(), Some(admin_id))]);
    }
}
