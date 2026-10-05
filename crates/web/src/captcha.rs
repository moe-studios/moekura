//! Asking for a captcha (Turnstile or hCaptcha) on sign-up and on new
//! users' comments (on the site and through the APIs alike), when
//! `auth.captcha` is configured and site settings turn it on. Both
//! services check tokens the same way.

use std::net::IpAddr;
use std::time::Duration;

use minijinja::{Value, context};
use moekura_core::config::CaptchaConfig;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::AppState;
use crate::auth::CurrentUser;

pub struct Captcha {
    config: CaptchaConfig,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct Verdict {
    success: bool,
    #[serde(default, rename = "error-codes")]
    error_codes: Vec<String>,
}

impl Captcha {
    pub fn new(config: CaptchaConfig) -> Self {
        moekura_storage::install_crypto_provider();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("moekura/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("the HTTP client configuration is valid");
        Self { config, client }
    }

    /// What a form needs to show the widget.
    pub fn widget(&self) -> Value {
        let provider = self.config.provider;
        context! {
            script => provider.script_url(),
            class => provider.widget_class(),
            site_key => self.config.site_key,
        }
    }

    /// Whether the service accepts `token` (from `ip`).
    async fn verify(&self, token: &str, ip: Option<IpAddr>) -> Result<bool, String> {
        let url = self
            .config
            .verify_url
            .as_ref()
            .map_or(self.config.provider.verify_url(), |u| u.as_str());
        // Built before any await: the serializer isn't Send.
        let body = {
            let mut body = url::form_urlencoded::Serializer::new(String::new());
            body.append_pair("secret", &self.config.secret_key)
                .append_pair("response", token)
                .append_pair("sitekey", &self.config.site_key);
            if let Some(ip) = ip {
                body.append_pair("remoteip", &ip.to_string());
            }
            body.finish()
        };
        let bytes = self
            .client
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        let verdict: Verdict = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if !verdict.success {
            tracing::info!(errors = ?verdict.error_codes, "captcha refused");
        }
        Ok(verdict.success)
    }

    /// Checks `token`; the error is what to tell the person.
    pub async fn check(&self, token: &str, ip: Option<IpAddr>) -> Result<(), String> {
        if token.trim().is_empty() {
            return Err("Complete the captcha first.".into());
        }
        match self.verify(token.trim(), ip).await {
            Ok(true) => Ok(()),
            Ok(false) => Err("The captcha wasn't solved; try again.".into()),
            Err(error) => {
                tracing::warn!(%error, "could not check a captcha");
                Err("The captcha couldn't be checked just now; try again in a moment.".into())
            }
        }
    }
}

/// The captcha for the sign-up form, if one is asked for there.
pub fn for_sign_up(state: &AppState) -> Option<&Captcha> {
    state
        .captcha
        .as_deref()
        .filter(|_| state.site.get().settings.captcha.sign_up)
}

/// The captcha for `current`'s comments, if their account is new enough
/// that one is asked for.
pub fn for_comment<'a>(state: &'a AppState, current: &CurrentUser) -> Option<&'a Captcha> {
    let days = state.site.get().settings.captcha.comment_account_days;
    let user = current.user.as_ref()?;
    let new = OffsetDateTime::now_utc() - user.created_at < time::Duration::days(days.into());
    state.captcha.as_deref().filter(|_| days > 0 && new)
}

#[cfg(test)]
pub(crate) mod test_service {
    //! A stand-in captcha service that accepts the token `good`.

    use axum::routing::post;
    use axum::{Form, Json, Router};
    use moekura_core::config::{CaptchaConfig, CaptchaProvider};
    use serde_json::json;

    async fn siteverify(Form(form): Form<Vec<(String, String)>>) -> Json<serde_json::Value> {
        let token = form
            .iter()
            .find(|(k, _)| k == "response")
            .map(|(_, v)| v.as_str());
        let secret = form
            .iter()
            .find(|(k, _)| k == "secret")
            .map(|(_, v)| v.as_str());
        Json(json!({ "success": token == Some("good") && secret == Some("shh") }))
    }

    /// Serves the stand-in and returns a configuration pointing at it.
    pub async fn start() -> CaptchaConfig {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/siteverify", post(siteverify)),
            )
            .await
            .unwrap();
        });
        CaptchaConfig {
            provider: CaptchaProvider::Turnstile,
            site_key: "site-key".into(),
            secret_key: "shh".into(),
            verify_url: Some(format!("http://{addr}/siteverify").parse().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use moekura_db::settings;
    use serde_json::json;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_config, test_state_with};

    async fn app(pool: &PgPool) -> TestApp {
        let mut config = test_config();
        config.auth.captcha = Some(super::test_service::start().await);
        TestApp::new(
            test_state_with(pool, config).await,
            crate::account::routes()
                .merge(crate::posts::routes())
                .merge(crate::comments::routes()),
        )
    }

    fn signup(name: &str, token: &str) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", name)
            .append_pair("password", "correct horse")
            .append_pair("password_confirm", "correct horse")
            .append_pair("cf-turnstile-response", token)
            .finish()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn sign_up_asks_for_a_captcha_once_turned_on(pool: PgPool) {
        let off = app(&pool).await;
        let form = off.get("/register", None).await;
        assert!(!form.body.contains("cf-turnstile"), "off by default");
        let response = off
            .post_form("/register", None, &[], &signup("early", ""))
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);

        settings::set(&pool, "captcha", json!({ "sign_up": true }))
            .await
            .unwrap();
        let app = app(&pool).await;
        let form = app.get("/register", None).await;
        assert!(
            form.body
                .contains("class=\"cf-turnstile\" data-sitekey=\"site-key\""),
            "{}",
            form.body
        );
        let full = app.get_full("/register").await;
        let csp = full.headers()["content-security-policy"].to_str().unwrap();
        assert!(
            csp.contains("script-src 'self' https://challenges.cloudflare.com"),
            "{csp}"
        );
        for token in ["", "bad"] {
            let refused = app
                .post_form("/register", None, &[], &signup("alice", token))
                .await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{token}");
            assert!(refused.body.contains("captcha"), "{}", refused.body);
        }
        let response = app
            .post_form("/register", None, &[], &signup("alice", "good"))
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn new_accounts_solve_one_to_comment(pool: PgPool) {
        settings::set(&pool, "captcha", json!({ "comment_account_days": 7 }))
            .await
            .unwrap();
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let old = session_for(&pool, "old", SystemRole::Member).await;
        sqlx::query("UPDATE users SET created_at = now() - interval '30 days' WHERE name = 'old'")
            .execute(&pool)
            .await
            .unwrap();
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
             VALUES ($1, sha256($1::text::bytea), substring(sha256($1::text::bytea) FROM 1 FOR 16), 'png', 10, 10, 1, 'original/aa/aa/x.png')",
        )
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
        let url = format!("/posts/{post}/comments");
        let page = app.get(&format!("/posts/{post}"), Some(&alice)).await;
        assert!(page.body.contains("cf-turnstile"), "{}", page.body);
        let page = app.get(&format!("/posts/{post}"), Some(&old)).await;
        assert!(!page.body.contains("cf-turnstile"));
        let refused = app.post_form(&url, Some(&alice), &[], "body=hello").await;
        assert!(
            refused.body.contains("Complete the captcha first."),
            "{}",
            refused.body
        );
        let posted = app
            .post_form(
                &url,
                Some(&alice),
                &[],
                "body=hello&cf-turnstile-response=good",
            )
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let posted = app.post_form(&url, Some(&old), &[], "body=hi").await;
        assert_eq!(
            posted.status,
            StatusCode::SEE_OTHER,
            "older accounts needn't"
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_apis_ask_new_accounts_too(pool: PgPool) {
        settings::set(&pool, "captcha", json!({ "comment_account_days": 7 }))
            .await
            .unwrap();
        let mut config = test_config();
        config.auth.captcha = Some(super::test_service::start().await);
        let app = TestApp::new(
            test_state_with(&pool, config).await,
            crate::api::routes(1024 * 1024).merge(crate::danbooru::routes(1024 * 1024)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();

        let api = format!("/api/v1/posts/{post}/comments");
        let refused = app
            .json("POST", &api, Some(&alice), Some(json!({ "body": "hi" })))
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("captcha"), "{}", refused.body);
        let wrong = app
            .json(
                "POST",
                &api,
                Some(&alice),
                Some(json!({ "body": "hi", "captcha": "bad" })),
            )
            .await;
        assert_eq!(wrong.status, StatusCode::BAD_REQUEST, "{}", wrong.body);
        let posted = app
            .json(
                "POST",
                &api,
                Some(&alice),
                Some(json!({ "body": "hi", "captcha": "good" })),
            )
            .await;
        assert_eq!(posted.status, StatusCode::CREATED, "{}", posted.body);

        let danbooru = format!("comment[post_id]={post}&comment[body]=hello");
        let refused = app
            .post_form("/comments.json", Some(&alice), &[], &danbooru)
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        let posted = app
            .post_form(
                "/comments.json",
                Some(&alice),
                &[],
                &format!("{danbooru}&captcha=good"),
            )
            .await;
        assert_eq!(posted.status, StatusCode::CREATED, "{}", posted.body);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM comments")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
    }
}
