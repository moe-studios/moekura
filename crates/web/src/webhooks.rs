//! Outgoing webhooks: telling them about events as they happen, and the
//! admin pages that manage them.

use axum::Router;
use axum::extract::{Form, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::posts::Rating;
use moekura_core::tokens::NewToken;
use moekura_core::webhooks::{Event, Format, discord};
use moekura_db::webhooks::{self, Fields, Webhook};
use serde::Deserialize;
use serde_json::json;
use time::format_description::well_known::Rfc3339;

use crate::AppState;
use crate::api::absolute_url;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

/// Deliveries listed on a webhook's page.
const LOG_SIZE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/webhooks", get(index).post(create))
        .route("/admin/webhooks/{id}", get(show).post(update))
        .route("/admin/webhooks/{id}/test", post(test))
        .route("/admin/webhooks/{id}/secret", post(new_secret))
        .route("/admin/webhooks/{id}/delete", post(delete))
}

/// Queues `event` with `data` for every webhook that wants it. Failing to
/// queue is logged rather than failing what already happened.
pub(crate) async fn emit(state: &AppState, event: Event, data: serde_json::Value) {
    let result = async {
        let mut tx = state.db.primary().begin().await?;
        let queued = webhooks::emit(&mut tx, event, &data, None).await?;
        tx.commit().await?;
        Ok::<_, sqlx::Error>(queued.len())
    }
    .await;
    match result {
        Ok(0) => {}
        Ok(n) => tracing::debug!(event = event.as_str(), deliveries = n, "webhooks queued"),
        Err(error) => tracing::warn!(%error, event = event.as_str(), "could not queue webhooks"),
    }
}

/// What a post event says about post `id`.
async fn post_data(state: &AppState, id: i64) -> Option<serde_json::Value> {
    let db = state.db.primary();
    let post = moekura_db::posts::by_id(db, id).await.ok()??;
    let mut tags: Vec<String> = moekura_db::tags::by_ids(db, &post.tag_ids)
        .await
        .ok()?
        .into_iter()
        .map(|t| t.name)
        .collect();
    tags.sort();
    let uploader = match post.uploader_id {
        Some(user) => moekura_db::users::by_id(db, user)
            .await
            .ok()?
            .map(|u| u.name),
        None => None,
    };
    // Only what visitors may see, so a Discord channel never shows more.
    let visitors = &state.site.get().settings.visitor_ratings;
    let image = if post.status.as_str() != "deleted"
        && !state.is_private()
        && (visitors.is_empty() || visitors.contains(&post.rating))
    {
        crate::previews::any_post_image(state, db, post.id)
            .await
            .ok()
            .flatten()
            .map(|image| image.url)
    } else {
        None
    };
    Some(json!({
        "post_id": post.id,
        "url": absolute_url(state, &format!("/posts/{}", post.id)),
        "status": post.status.as_str(),
        "rating": post.rating.code(),
        "source": post.source,
        "tags": tags,
        "uploader_url": uploader.as_deref().map(|name| user_url(state, name)),
        "uploader": uploader,
        "image_url": image,
        "created_at": post.created_at.format(&Rfc3339).unwrap_or_default(),
    }))
}

/// Tells webhooks about post `id`, with `extra` fields (a reason).
pub(crate) async fn emit_post(state: &AppState, event: Event, id: i64, extra: serde_json::Value) {
    if let Some(mut data) = post_data(state, id).await {
        if let (Some(data), serde_json::Value::Object(extra)) = (data.as_object_mut(), extra) {
            data.extend(extra);
        }
        emit(state, event, data).await;
    }
}

/// Tells webhooks about comment `id`.
pub(crate) async fn emit_comment(state: &AppState, id: i64) {
    let Ok(Some(comment)) = moekura_db::comments::by_id(state.db.primary(), id).await else {
        return;
    };
    let data = json!({
        "comment_id": comment.id,
        "post_id": comment.post_id,
        "url": absolute_url(state, &crate::comments::url(&comment)),
        "creator_url": comment.creator_name.as_deref().map(|name| user_url(state, name)),
        "creator": comment.creator_name,
        "body": comment.body,
        "created_at": comment.created_at.format(&Rfc3339).unwrap_or_default(),
    });
    emit(state, Event::CommentCreated, data).await;
}

/// Tells webhooks someone registered.
pub(crate) async fn emit_user(state: &AppState, user: &moekura_db::users::User) {
    let data = json!({
        "user_id": user.id,
        "name": user.name,
        "url": user_url(state, &user.name),
        "status": user.status.as_str(),
    });
    emit(state, Event::UserRegistered, data).await;
}

/// The absolute URL of user `name`'s page.
fn user_url(state: &AppState, name: &str) -> String {
    let name: String = url::form_urlencoded::byte_serialize(name.as_bytes()).collect();
    absolute_url(state, &format!("/users/{name}"))
}

/// A new webhook secret.
fn secret() -> String {
    format!("whsec_{}", NewToken::generate().token)
}

/// Checks a webhook's URL: a web address, not to a private address
/// unless the config allows it.
fn check_url(state: &AppState, raw: &str) -> Result<String, String> {
    let url = url::Url::parse(raw.trim()).map_err(|_| "That isn't a valid URL.".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Webhooks go to http or https URLs.".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "Put credentials in the receiver's check of the signature, not the URL.".into(),
        );
    }
    let allowed = url.host().is_some_and(|host| {
        moekura_net::host_allowed(host, state.config.webhooks.allow_private_addresses)
    });
    if !allowed {
        return Err(
            "That address isn't on the public internet (see webhooks.allow_private_addresses)."
                .into(),
        );
    }
    Ok(url.to_string())
}

fn events_context(chosen: &[String]) -> Vec<Value> {
    Event::SUBSCRIBABLE
        .iter()
        .map(|e| {
            context! {
                name => e.as_str(),
                label => e.label(),
                chosen => chosen.iter().any(|c| c == e.as_str()),
            }
        })
        .collect()
}

fn hook_context(hook: &Webhook) -> Value {
    context! {
        id => hook.id,
        url => hook.url,
        description => hook.description,
        events => hook.events,
        enabled => hook.is_enabled,
        format => hook.format,
    }
}

/// The format choices, with `chosen` (`auto`, or a format's name)
/// selected.
fn formats_context(chosen: &str) -> Vec<Value> {
    std::iter::once("auto")
        .chain(Format::ALL.iter().map(|f| f.as_str()))
        .map(|name| context! { name => name, chosen => name == chosen })
        .collect()
}

/// The rating choices for Discord images, with `chosen` ticked.
fn ratings_context(chosen: &[String]) -> Vec<Value> {
    Rating::ALL
        .iter()
        .map(|r| {
            context! {
                code => r.code(),
                label => r.label(),
                chosen => chosen.iter().any(|c| c == r.code()),
            }
        })
        .collect()
}

/// The Discord settings part of the form.
fn discord_context(format: &str, ratings: &[String], username: &str, avatar_url: &str) -> Value {
    context! {
        formats => formats_context(format),
        ratings => ratings_context(ratings),
        username => username,
        avatar_url => avatar_url,
    }
}

async fn render_index(
    page: &Page,
    form: &HookForm,
    error: Option<String>,
) -> Result<Response, AppError> {
    let hooks = webhooks::list(page.state().db.primary()).await?;
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let chosen = form.events();
    let format = form.format.as_deref().unwrap_or("auto");
    // A new form shows images for the usual ratings.
    let ratings = if form.url.is_empty() {
        vec!["g".to_owned(), "s".to_owned()]
    } else {
        form.image_ratings()
    };
    Ok(page.render_with_status(
        status,
        "admin_webhooks.html",
        context! {
            hooks => hooks.iter().map(hook_context).collect::<Vec<_>>(),
            form => context! { url => form.url, description => form.description, events => events_context(&chosen) },
            discord => discord_context(format, &ratings, &form.username, &form.avatar_url),
            error => error,
        },
    ))
}

async fn index(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    render_index(&page, &HookForm::default(), None).await
}

/// The webhook form: url, description, one field per ticked event,
/// `enabled` when ticked, the format (`auto` to tell from the URL), and
/// for Discord a name, an avatar and one `image-{rating}` field per
/// rating whose images may show.
#[derive(Debug, Default, Deserialize)]
struct HookForm {
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    enabled: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    username: String,
    #[serde(default)]
    avatar_url: String,
    #[serde(flatten)]
    rest: std::collections::HashMap<String, String>,
}

impl HookForm {
    fn events(&self) -> Vec<String> {
        Event::SUBSCRIBABLE
            .iter()
            .map(|e| e.as_str().to_owned())
            .filter(|name| self.rest.contains_key(name))
            .collect()
    }

    fn image_ratings(&self) -> Vec<String> {
        Rating::ALL
            .iter()
            .map(|r| r.code().to_owned())
            .filter(|code| self.rest.contains_key(&format!("image-{code}")))
            .collect()
    }
}

fn check_form(state: &AppState, form: &HookForm) -> Result<Fields, String> {
    let url = check_url(state, &form.url)?;
    let description = form.description.trim().to_owned();
    if description.chars().count() > 200 {
        return Err("The description may be at most 200 characters.".into());
    }
    let events = form.events();
    if events.is_empty() {
        return Err("Choose at least one event.".into());
    }
    let format = match form.format.as_deref().unwrap_or("auto") {
        "auto" => Format::detect(&url),
        name => Format::parse(name).ok_or("Choose a format.")?,
    };
    let username = form.username.trim().to_owned();
    if username.chars().count() > discord::USERNAME_LEN {
        return Err(format!(
            "The Discord name may be at most {} characters.",
            discord::USERNAME_LEN
        ));
    }
    // Discord refuses these names.
    let lower = username.to_lowercase();
    if lower.contains("discord") || lower.contains("clyde") {
        return Err("Discord doesn't allow names with \"discord\" or \"clyde\" in them.".into());
    }
    let avatar_url = form.avatar_url.trim().to_owned();
    if !avatar_url.is_empty()
        && !url::Url::parse(&avatar_url)
            .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && avatar_url.len() <= 2048)
    {
        return Err("The avatar should be the URL of an image.".into());
    }
    Ok(Fields {
        url,
        description,
        events,
        format,
        image_ratings: form.image_ratings(),
        username,
        avatar_url,
    })
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<HookForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let fields = match check_form(page.state(), &form) {
        Ok(checked) => checked,
        Err(message) => return render_index(&page, &form, Some(message)).await,
    };
    let id = webhooks::create(page.state().db.primary(), &fields, &secret()).await?;
    tracing::info!(
        id,
        url = fields.url,
        format = fields.format.as_str(),
        "webhook added"
    );
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/admin/webhooks/{id}")),
    )
        .into_response())
}

async fn render_show(
    page: &Page,
    hook: &Webhook,
    error: Option<String>,
) -> Result<Response, AppError> {
    let deliveries = webhooks::deliveries(page.state().db.primary(), hook.id, LOG_SIZE).await?;
    let rows: Vec<Value> = deliveries
        .iter()
        .map(|d| {
            context! {
                id => d.id,
                event => d.event,
                status => d.status,
                attempts => d.attempts,
                response_status => d.response_status,
                response => d.response,
                when => d.created_at.format(&Rfc3339).unwrap_or_default(),
            }
        })
        .collect();
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    Ok(page.render_with_status(
        status,
        "admin_webhook.html",
        context! {
            hook => hook_context(hook),
            secret => hook.secret,
            events => events_context(&hook.events),
            discord => discord_context(&hook.format, &hook.image_ratings, &hook.username, &hook.avatar_url),
            deliveries => rows,
            error => error,
        },
    ))
}

async fn find(page: &Page, id: i32) -> Result<Webhook, AppError> {
    page.current.require(Permission::ManageSettings)?;
    webhooks::by_id(page.state().db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)
}

async fn show(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    let hook = find(&page, id).await?;
    render_show(&page, &hook, None).await
}

async fn update(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<HookForm>,
) -> Result<Response, AppError> {
    let hook = find(&page, id).await?;
    let fields = match check_form(page.state(), &form) {
        Ok(checked) => checked,
        Err(message) => return render_show(&page, &hook, Some(message)).await,
    };
    webhooks::update(
        page.state().db.primary(),
        id,
        &fields,
        form.enabled.is_some(),
    )
    .await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/admin/webhooks/{id}")),
    )
        .into_response())
}

async fn test(page: Page, jar: CookieJar, Path(id): Path<i32>) -> Result<Response, AppError> {
    let hook = find(&page, id).await?;
    let data = json!({
        "message": "A test from Moekura.",
        "site": absolute_url(page.state(), "/"),
        "site_name": page.state().site.get().settings.site_name,
    });
    let mut tx = page.state().db.primary().begin().await?;
    webhooks::emit(&mut tx, Event::Ping, &data, Some(hook.id)).await?;
    tx.commit().await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/admin/webhooks/{id}")),
    )
        .into_response())
}

async fn new_secret(page: Page, jar: CookieJar, Path(id): Path<i32>) -> Result<Response, AppError> {
    find(&page, id).await?;
    webhooks::set_secret(page.state().db.primary(), id, &secret()).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/admin/webhooks/{id}")),
    )
        .into_response())
}

async fn delete(page: Page, jar: CookieJar, Path(id): Path<i32>) -> Result<Response, AppError> {
    find(&page, id).await?;
    webhooks::delete(page.state().db.primary(), id).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/admin/webhooks"),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, fixture, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn admins_manage_webhooks_that_hear_events(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            routes()
                .merge(crate::upload::routes(max))
                .merge(crate::comments::routes()),
        );
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let member = session_for(&pool, "alice", SystemRole::Member).await;
        assert_eq!(
            app.get("/admin/webhooks", Some(&member)).await.status,
            StatusCode::FORBIDDEN
        );
        let private = app
            .post_form(
                "/admin/webhooks",
                Some(&admin),
                &[],
                "url=http%3A%2F%2F127.0.0.1%2Fhook&post.created=on",
            )
            .await;
        assert_eq!(private.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            private.body.contains("isn&#x27;t on the public internet"),
            "{}",
            private.body
        );
        let none = app
            .post_form(
                "/admin/webhooks",
                Some(&admin),
                &[],
                "url=https%3A%2F%2Fhooks.example%2Fa",
            )
            .await;
        assert!(none.body.contains("Choose at least one event."));

        let made = app
            .post_form(
                "/admin/webhooks",
                Some(&admin),
                &[],
                "url=https%3A%2F%2Fhooks.example%2Fa&description=Bot&post.created=on&comment.created=on&enabled=on",
            )
            .await;
        assert_eq!(made.status, StatusCode::SEE_OTHER, "{}", made.body);
        let url = made.location.unwrap();
        let page = app.get(&url, Some(&admin)).await;
        assert!(page.body.contains("whsec_"), "{}", page.body);

        // Uploading and commenting queue deliveries.
        let uploaded = app
            .post_multipart(
                "/upload",
                Some(&member),
                &[("rating", "g".to_owned()), ("tags", "cat".to_owned())],
                Some(("a.png", &fixture::png(20, 20))),
            )
            .await;
        let post: i64 = uploaded.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        app.post_form(
            &format!("/posts/{post}/comments"),
            Some(&member),
            &[],
            "body=Nice",
        )
        .await;
        let events: Vec<(String, serde_json::Value)> =
            sqlx::query_as("SELECT event, payload FROM webhook_deliveries ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(events[0].0, "post.created");
        assert_eq!(events[0].1["tags"], json!(["cat"]));
        assert_eq!(events[0].1["uploader"], json!("alice"));
        assert_eq!(events[1].0, "comment.created");
        assert_eq!(events[1].1["body"], json!("Nice"));

        // A test goes to this webhook even when it's off.
        app.post_form(
            &url,
            Some(&admin),
            &[],
            "url=https%3A%2F%2Fhooks.example%2Fa&post.created=on",
        )
        .await;
        app.post(&format!("{url}/test"), Some(&admin), &[]).await;
        let last: String =
            sqlx::query_scalar("SELECT event FROM webhook_deliveries ORDER BY id DESC LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(last, "ping");
        assert!(
            app.get(&url, Some(&admin))
                .await
                .body
                .contains("<code>ping</code>")
        );
        app.post(&format!("{url}/delete"), Some(&admin), &[]).await;
        assert!(webhooks::list(&pool).await.unwrap().is_empty());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn discord_webhooks(pool: PgPool) {
        moekura_db::settings::set(&pool, "visitor_ratings", json!(["g", "s"]))
            .await
            .unwrap();
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(state, routes().merge(crate::upload::routes(max)));
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let member = session_for(&pool, "alice", SystemRole::Member).await;

        let form = app.get("/admin/webhooks", Some(&admin)).await;
        assert!(
            form.body.contains(r#"name="image-g" checked"#),
            "{}",
            form.body
        );
        assert!(!form.body.contains(r#"name="image-e" checked"#));
        let refused = app
            .post_form(
                "/admin/webhooks",
                Some(&admin),
                &[],
                "url=https%3A%2F%2Fdiscord.com%2Fapi%2Fwebhooks%2F1%2Ftok&post.created=on&username=Discord+bot",
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);

        // Told from the URL; images for general and questionable posts.
        let made = app
            .post_form(
                "/admin/webhooks",
                Some(&admin),
                &[],
                "url=https%3A%2F%2Fdiscord.com%2Fapi%2Fwebhooks%2F1%2Ftok&post.created=on&format=auto&username=Booru&image-g=on&image-q=on",
            )
            .await;
        assert_eq!(made.status, StatusCode::SEE_OTHER, "{}", made.body);
        let url = made.location.unwrap();
        let hook = &webhooks::list(&pool).await.unwrap()[0];
        assert_eq!(
            (hook.format.as_str(), hook.username.as_str()),
            ("discord", "Booru")
        );
        assert_eq!(hook.image_ratings, ["g", "q"]);
        let page = app.get(&url, Some(&admin)).await;
        assert!(!page.body.contains("whsec_"), "the secret isn't used");
        assert!(page.body.contains(r#"<option value="discord" selected>"#));

        // Images only for ratings visitors see.
        for rating in ["g", "q"] {
            app.post_multipart(
                "/upload",
                Some(&member),
                &[("rating", rating.to_owned()), ("tags", "cat".to_owned())],
                Some((
                    "a.png",
                    &fixture::png(if rating == "q" { 24 } else { 20 }, 20),
                )),
            )
            .await;
        }
        let payloads: Vec<serde_json::Value> =
            sqlx::query_scalar("SELECT payload FROM webhook_deliveries ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(payloads.len(), 2);
        assert!(payloads[0]["image_url"].is_string(), "{}", payloads[0]);
        assert_eq!(payloads[1]["image_url"], json!(null));
        assert!(
            payloads[0]["uploader_url"]
                .as_str()
                .unwrap()
                .ends_with("/users/alice")
        );

        // Choosing by hand.
        app.post_form(
            &url,
            Some(&admin),
            &[],
            "url=https%3A%2F%2Fdiscord.com%2Fapi%2Fwebhooks%2F1%2Ftok&post.created=on&format=moekura&enabled=on",
        )
        .await;
        let hook = &webhooks::list(&pool).await.unwrap()[0];
        assert_eq!(hook.format, "moekura");
        assert!(hook.image_ratings.is_empty());
    }
}
