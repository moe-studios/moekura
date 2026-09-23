//! Creating and revoking API keys, from account settings.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use serde::Deserialize;
use time::OffsetDateTime;
use uwu_db::api_keys::{self, CreateError, NAME_MAX_LEN};

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings/api-keys", get(index).post(create))
        .route("/settings/api-keys/{id}/revoke", post(revoke))
}

/// Expiry choices: form value, label, days (`None` for never).
const EXPIRIES: [(&str, &str, Option<i64>); 4] = [
    ("never", "Never", None),
    ("30", "In 30 days", Some(30)),
    ("90", "In 90 days", Some(90)),
    ("365", "In a year", Some(365)),
];

#[derive(Debug, Default, Deserialize)]
struct CreateForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    expires: String,
}

/// The page. `created` is a key just made, shown this once; `failed` a
/// rejected form and why.
async fn render(
    page: &Page,
    created: Option<(&str, &str)>,
    failed: Option<(&CreateForm, String)>,
) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let now = OffsetDateTime::now_utc();
    let keys: Vec<Value> = api_keys::list(page.state().db.primary(), user.id)
        .await?
        .into_iter()
        .map(|k| {
            context! {
                id => k.id,
                name => k.name,
                prefix => k.prefix,
                created => k.created_at.date().to_string(),
                last_used => k.last_used_at.map(|t| t.date().to_string()),
                expires => k.expires_at.map(|t| t.date().to_string()),
                expired => k.expires_at.is_some_and(|t| t <= now),
            }
        })
        .collect();
    let status = if failed.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let (form, error) = match failed {
        Some((form, error)) => (Some(form), Some(error)),
        None => (None, None),
    };
    let mut response = page.render_with_status(
        status,
        "api_keys.html",
        context! {
            keys => keys,
            created => created.map(|(name, key)| context! { name => name, key => key }),
            error => error,
            name => form.map(|f| f.name.clone()).unwrap_or_default(),
            expires => form.map_or("never", |f| f.expires.as_str()),
            expiries => EXPIRIES
                .iter()
                .map(|(value, label, _)| context! { value => value, label => label })
                .collect::<Vec<_>>(),
            name_max => NAME_MAX_LEN,
            docs_url => format!("{}/openapi.json", crate::api::BASE),
        },
    );
    // The page may show a new key: keep it out of every cache.
    response
        .headers_mut()
        .insert(CACHE_CONTROL, "no-store".parse().expect("valid header"));
    Ok(response)
}

async fn index(page: Page) -> Result<Response, AppError> {
    render(&page, None, None).await
}

async fn create(page: Page, Form(form): Form<CreateForm>) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let name = form.name.trim();
    let invalid = |message: &str| render(&page, None, Some((&form, message.to_owned())));
    if name.is_empty() {
        return invalid("Give the key a name.").await;
    }
    if name.chars().count() > NAME_MAX_LEN {
        return invalid(&format!(
            "The name may be at most {NAME_MAX_LEN} characters."
        ))
        .await;
    }
    let Some((_, _, days)) = EXPIRIES.iter().find(|(value, ..)| *value == form.expires) else {
        return Err(AppError::BadRequest("Unknown expiry".into()));
    };
    let expires_at = days.map(|d| OffsetDateTime::now_utc() + time::Duration::days(d));
    match api_keys::create(page.state().db.primary(), user.id, name, expires_at).await {
        Ok(key) => {
            tracing::info!(user = %user.name, key = name, "API key created");
            render(&page, Some((name, &key)), None).await
        }
        Err(CreateError::TooMany) => invalid(&CreateError::TooMany.to_string()).await,
        Err(CreateError::Db(error)) => Err(error.into()),
    }
}

async fn revoke(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    if !api_keys::revoke(page.state().db.primary(), user.id, id).await? {
        return Err(AppError::NotFound);
    }
    tracing::info!(user = %user.name, key_id = id, "API key revoked");
    Ok((
        flash::set(jar, Flash::ApiKeyRevoked),
        Redirect::to("/settings/api-keys"),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use sqlx::PgPool;
    use uwu_core::permissions::SystemRole;

    use crate::test_support::{TestApp, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        let routes = super::routes().merge(crate::api::routes());
        TestApp::new(test_state(pool).await, routes)
    }

    /// The key shown on the page after creating one.
    fn shown_key(body: &str) -> String {
        let start = body.find("uwu_").expect("a key is shown");
        body[start..start + 68].to_owned()
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn keys_are_created_used_and_revoked(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;

        let created = app
            .post_form(
                "/settings/api-keys",
                Some(&alice),
                &[],
                "name=my+script&expires=30",
            )
            .await;
        assert_eq!(created.status, StatusCode::OK, "{}", created.body);
        let key = shown_key(&created.body);
        assert!(created.body.contains("my script"));

        // Shown once: the list has only its prefix.
        let page = app.get("/settings/api-keys", Some(&alice)).await;
        assert!(!page.body.contains(&key));
        assert!(page.body.contains(&key[..12]), "{}", page.body);

        let me = app
            .get_with_headers("/api/v1/me", &[("authorization", &format!("Bearer {key}"))])
            .await;
        assert_eq!(me.status, StatusCode::OK, "{}", me.body);
        assert!(me.body.contains("\"name\":\"alice\""), "{}", me.body);

        let id: i64 = sqlx::query_scalar("SELECT id FROM api_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        // Nobody else can revoke it.
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let path = format!("/settings/api-keys/{id}/revoke");
        assert_eq!(
            app.post(&path, Some(&bob), &[]).await.status,
            StatusCode::NOT_FOUND
        );
        let revoked = app.post(&path, Some(&alice), &[]).await;
        assert_eq!(revoked.status, StatusCode::SEE_OTHER);

        let refused = app
            .get_with_headers("/api/v1/me", &[("authorization", &format!("Bearer {key}"))])
            .await;
        assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
        assert!(refused.body.contains("invalid, revoked or expired"));
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn keys_carry_bans_and_private_access(pool: PgPool) {
        use uwu_core::permissions::Permissions;
        // A private site.
        sqlx::query("UPDATE roles SET permissions = $1 WHERE system_key = 'anonymous'")
            .bind(Permissions::NONE.to_db())
            .execute(&pool)
            .await
            .unwrap();
        let app = app(&pool).await;
        session_for(&pool, "alice", SystemRole::Member).await;
        let alice = uwu_db::users::by_name(&pool, "alice")
            .await
            .unwrap()
            .unwrap();
        let key = uwu_db::api_keys::create(&pool, alice.id, "bot", None)
            .await
            .unwrap();
        let bearer = format!("Bearer {key}");
        let auth = [("authorization", bearer.as_str())];

        assert_eq!(
            app.get_with_headers("/api/v1/posts", &auth).await.status,
            StatusCode::OK
        );
        // Other schemes (a proxy's Basic auth) are ignored, not refused.
        let basic = app
            .get_with_headers("/api/v1/posts", &[("authorization", "Basic dXNlcjpwYXNz")])
            .await;
        assert_eq!(basic.status, StatusCode::UNAUTHORIZED);
        assert!(
            basic.body.contains("You need to log in first"),
            "{}",
            basic.body
        );

        sqlx::query("INSERT INTO bans (user_id, reason) VALUES ($1, 'spam')")
            .bind(alice.id)
            .execute(&pool)
            .await
            .unwrap();
        let me = app.get_with_headers("/api/v1/me", &auth).await;
        let me: serde_json::Value = serde_json::from_str(&me.body).unwrap();
        assert_eq!(me["ban"]["reason"], "spam");
        assert_eq!(me["permissions"], serde_json::json!([]));
        // Banned users keep only what visitors may do: here, nothing.
        assert_eq!(
            app.get_with_headers("/api/v1/posts", &auth).await.status,
            StatusCode::FORBIDDEN
        );
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn bad_names_are_explained(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let response = app
            .post_form(
                "/settings/api-keys",
                Some(&alice),
                &[],
                "name=+&expires=never",
            )
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(response.body.contains("Give the key a name."));
        let visitor = app.get("/settings/api-keys", None).await;
        assert_eq!(
            visitor.location.as_deref(),
            Some("/login?next=%2Fsettings%2Fapi-keys")
        );
    }
}
