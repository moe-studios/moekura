//! Helpers for driving the router in tests.

use axum::Router;
use axum::body::Body;
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;
use uwuu_core::config::Config;
use uwuu_db::Db;
use uwuu_db::site_cache::SiteCache;

use crate::auth::SESSION_COOKIE;
use crate::{AppState, with_middleware};

pub async fn test_state(pool: &PgPool) -> AppState {
    AppState::new(
        Config::default(),
        Db::from_pools(pool.clone(), vec![]),
        SiteCache::load(pool).await.unwrap(),
    )
    .unwrap()
}

pub struct TestApp {
    router: Router,
}

pub struct TestResponse {
    pub status: StatusCode,
    pub body: String,
    pub set_cookie: Vec<String>,
    pub location: Option<String>,
}

impl TestResponse {
    /// The session token from a `Set-Cookie` that sets (not clears) it.
    pub fn session_cookie(&self) -> Option<String> {
        let prefix = format!("{SESSION_COOKIE}=");
        self.set_cookie
            .iter()
            .filter_map(|c| c.strip_prefix(&prefix))
            .map(|rest| rest.split(';').next().unwrap_or_default().to_owned())
            .find(|token| !token.is_empty())
    }
}

impl TestApp {
    pub fn new(state: AppState, routes: Router<AppState>) -> Self {
        Self {
            router: with_middleware(routes, state),
        }
    }

    /// The raw response, for checking headers.
    pub async fn get_full(&self, path: &str) -> axum::response::Response {
        let request = Request::get(path).body(Body::empty()).unwrap();
        self.router.clone().oneshot(request).await.unwrap()
    }

    /// A GET carrying an arbitrary `name=value` cookie.
    pub async fn get_with_cookie(&self, path: &str, cookie: &str) -> TestResponse {
        let builder = Request::get(path).header(COOKIE, cookie);
        self.send(builder, None, Body::empty()).await
    }

    pub async fn get(&self, path: &str, session: Option<&str>) -> TestResponse {
        self.send(Request::get(path), session, Body::empty()).await
    }

    /// A form post; `headers` lets tests set `Origin` / `Sec-Fetch-Site`.
    pub async fn post(
        &self,
        path: &str,
        session: Option<&str>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        self.post_form(path, session, headers, "").await
    }

    pub async fn post_form(
        &self,
        path: &str,
        session: Option<&str>,
        headers: &[(&str, &str)],
        form: &str,
    ) -> TestResponse {
        let mut builder =
            Request::post(path).header("content-type", "application/x-www-form-urlencoded");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        self.send(builder, session, Body::from(form.to_owned()))
            .await
    }

    async fn send(
        &self,
        mut builder: axum::http::request::Builder,
        session: Option<&str>,
        body: Body,
    ) -> TestResponse {
        if let Some(token) = session {
            builder = builder.header(COOKIE, format!("{SESSION_COOKIE}={token}"));
        }
        let response = self
            .router
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let set_cookie = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect();
        let location = response
            .headers()
            .get("location")
            .map(|v| v.to_str().unwrap().to_owned());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        TestResponse {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
            set_cookie,
            location,
        }
    }
}
