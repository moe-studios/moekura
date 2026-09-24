//! Helpers for driving the router in tests.

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;
use uwu_core::config::Config;
use uwu_db::Db;
use uwu_db::site_cache::SiteCache;

use crate::auth::SESSION_COOKIE;
use crate::{AppState, with_middleware};

/// Defaults, with file storage and scratch space in a fresh temporary
/// directory.
pub fn test_config() -> Config {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("uwu-web-test-{}-{n}", std::process::id()));
    let mut config = Config::default();
    config.storage.path = root.join("storage");
    config.media.work_dir = Some(root.join("work"));
    config
}

pub async fn test_state(pool: &PgPool) -> AppState {
    test_state_with(pool, test_config()).await
}

/// State whose database has one "replica": the same database, which is
/// enough to exercise replica routing.
pub async fn test_state_with_replica(pool: &PgPool) -> AppState {
    AppState::new(
        test_config(),
        Db::from_pools(pool.clone(), vec![pool.clone()]),
        SiteCache::load(pool).await.unwrap(),
        [7; 32],
    )
    .unwrap()
}

pub async fn test_state_with(pool: &PgPool, config: Config) -> AppState {
    AppState::new(
        config,
        Db::from_pools(pool.clone(), vec![]),
        SiteCache::load(pool).await.unwrap(),
        [7; 32],
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
    pub retry_after: Option<u64>,
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

    /// Requests appear to arrive over a connection from `peer`.
    pub fn with_peer(state: AppState, routes: Router<AppState>, peer: SocketAddr) -> Self {
        Self {
            router: with_middleware(routes, state).layer(MockConnectInfo(peer)),
        }
    }

    /// The raw response, for checking headers.
    pub async fn get_full(&self, path: &str) -> axum::response::Response {
        let request = Request::get(path).body(Body::empty()).unwrap();
        self.router.clone().oneshot(request).await.unwrap()
    }

    /// A `multipart/form-data` post with text `fields` and an optional
    /// `(file name, bytes)` in the `file` field.
    pub async fn post_multipart(
        &self,
        path: &str,
        session: Option<&str>,
        fields: &[(&str, String)],
        file: Option<(&str, &[u8])>,
    ) -> TestResponse {
        const BOUNDARY: &str = "uwu-test-boundary";
        let mut body = Vec::new();
        for (name, value) in fields {
            let part = format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            );
            body.extend_from_slice(part.as_bytes());
        }
        if let Some((file_name, bytes)) = file {
            let head = format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
                 filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            );
            body.extend_from_slice(head.as_bytes());
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
        let builder = Request::post(path).header("content-type", content_type);
        self.send(builder, session, Body::from(body)).await
    }

    /// A GET carrying an arbitrary `name=value` cookie.
    pub async fn get_with_cookie(&self, path: &str, cookie: &str) -> TestResponse {
        let builder = Request::get(path).header(COOKIE, cookie);
        self.send(builder, None, Body::empty()).await
    }

    /// A request with a JSON body (when given), as API clients send.
    pub async fn json(
        &self,
        method: &str,
        path: &str,
        session: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> TestResponse {
        let builder = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        self.send(builder, session, body).await
    }

    /// A GET with extra headers (`Authorization`, …) and no session.
    pub async fn get_with_headers(&self, path: &str, headers: &[(&str, &str)]) -> TestResponse {
        let mut builder = Request::get(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
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
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()?.parse().ok());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        TestResponse {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
            set_cookie,
            location,
            retry_after,
        }
    }
}

/// Logs a new user with `role` in and returns their session token.
pub async fn session_for(
    pool: &PgPool,
    name: &str,
    role: uwu_core::permissions::SystemRole,
) -> String {
    use uwu_db::users::{NewUser, UserStatus};
    let role_id = uwu_db::roles::by_system(pool, role).await.unwrap().id;
    let new = NewUser {
        name,
        email: None,
        password_hash: None,
        role_id,
        status: UserStatus::Active,
    };
    let user = uwu_db::users::insert(pool, new).await.unwrap();
    let session = uwu_db::sessions::NewSession {
        user_id: user.id,
        user_agent: None,
        ip: None,
    };
    let lifetime = uwu_db::sessions::Lifetime {
        idle: std::time::Duration::from_secs(3600),
        max: std::time::Duration::from_secs(3600),
    };
    uwu_db::sessions::create(pool, session, lifetime)
        .await
        .unwrap()
}

/// Media files made with ffmpeg on demand, so the repository carries no
/// binary fixtures.
pub mod fixture {
    use std::process::Command;

    fn encode(width: u32, height: u32, codec: &str) -> Vec<u8> {
        let source = format!("testsrc2=size={width}x{height}:duration=1");
        let output = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
            ])
            .args(["-frames:v", "1", "-c:v", codec, "-f", "image2pipe", "-"])
            .output()
            .expect("ffmpeg is needed for upload tests");
        assert!(output.status.success(), "ffmpeg failed");
        output.stdout
    }

    pub fn png(width: u32, height: u32) -> Vec<u8> {
        encode(width, height, "png")
    }
}
