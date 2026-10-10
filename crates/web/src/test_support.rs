//! Helpers for driving the router in tests.

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use moekura_core::config::Config;
use moekura_db::Db;
use moekura_db::site_cache::SiteCache;
use sqlx::PgPool;
use tower::ServiceExt;

use crate::auth::SESSION_COOKIE;
use crate::{AppState, with_middleware};

/// Defaults, with file storage and scratch space in a fresh temporary
/// directory.
pub fn test_config() -> Config {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("moekura-web-test-{}-{n}", std::process::id()));
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
    pub headers: axum::http::HeaderMap,
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

    /// Sends `request` as is and returns the raw response.
    pub async fn raw(&self, request: Request<Body>) -> axum::response::Response {
        self.router.clone().oneshot(request).await.unwrap()
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
        self.post_multipart_as(path, session, fields, "file", file)
            .await
    }

    /// [`Self::post_multipart`] with the file under `file_field`.
    pub async fn post_multipart_as(
        &self,
        path: &str,
        session: Option<&str>,
        fields: &[(&str, String)],
        file_field: &str,
        file: Option<(&str, &[u8])>,
    ) -> TestResponse {
        let files: Vec<(&str, &str, &[u8])> = file
            .map(|(name, bytes)| (file_field, name, bytes))
            .into_iter()
            .collect();
        self.post_multipart_files(path, session, fields, &files)
            .await
    }

    /// [`Self::post_multipart`] with any number of `(field, file name,
    /// bytes)` files.
    pub async fn post_multipart_files(
        &self,
        path: &str,
        session: Option<&str>,
        fields: &[(&str, String)],
        files: &[(&str, &str, &[u8])],
    ) -> TestResponse {
        const BOUNDARY: &str = "moekura-test-boundary";
        let mut body = Vec::new();
        for (name, value) in fields {
            let part = format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            );
            body.extend_from_slice(part.as_bytes());
        }
        for (file_field, file_name, bytes) in files {
            let head = format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{file_field}\"; \
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

    /// A JSON POST carrying an arbitrary `name=value` cookie.
    pub async fn json_with_cookie(
        &self,
        path: &str,
        cookie: &str,
        body: serde_json::Value,
    ) -> TestResponse {
        let builder = Request::post(path)
            .header("content-type", "application/json")
            .header(COOKIE, cookie);
        self.send(builder, None, Body::from(body.to_string())).await
    }

    /// A GET with extra headers (`Authorization`, …) and no session.
    pub async fn get_with_headers(&self, path: &str, headers: &[(&str, &str)]) -> TestResponse {
        let mut builder = Request::get(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        self.send(builder, None, Body::empty()).await
    }

    /// A GET asking for JSON, as the pages' scripts do.
    pub async fn get_json(&self, path: &str, session: Option<&str>) -> TestResponse {
        let builder = Request::get(path).header("accept", "application/json");
        self.send(builder, session, Body::empty()).await
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

    /// A form body sent with any method (`PUT`, `PATCH`, …).
    pub async fn form(
        &self,
        method: &str,
        path: &str,
        session: Option<&str>,
        form: &str,
    ) -> TestResponse {
        let builder = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/x-www-form-urlencoded");
        self.send(builder, session, Body::from(form.to_owned()))
            .await
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
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        TestResponse {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
            set_cookie,
            location,
            retry_after,
            headers,
        }
    }
}

/// Logs a new user with `role` in and returns their session token.
pub async fn session_for(
    pool: &PgPool,
    name: &str,
    role: moekura_core::permissions::SystemRole,
) -> String {
    use moekura_db::users::{NewUser, UserStatus};
    let role_id = moekura_db::roles::by_system(pool, role).await.unwrap().id;
    let new = NewUser {
        name,
        email: None,
        password_hash: None,
        role_id,
        status: UserStatus::Active,
    };
    let user = moekura_db::users::insert(pool, new).await.unwrap();
    let session = moekura_db::sessions::NewSession {
        user_id: user.id,
        user_agent: None,
        ip: None,
    };
    let lifetime = moekura_db::sessions::Lifetime {
        idle: std::time::Duration::from_secs(3600),
        max: std::time::Duration::from_secs(3600),
    };
    moekura_db::sessions::create(pool, session, lifetime)
        .await
        .unwrap()
}

/// A member with a password and email, and a session for them.
pub async fn member(pool: &PgPool, name: &str, email: &str) -> (moekura_db::users::User, String) {
    let role = moekura_db::roles::by_system(pool, moekura_core::permissions::SystemRole::Member)
        .await
        .unwrap();
    let user = moekura_db::accounts::create(
        pool,
        moekura_db::accounts::NewAccount {
            name,
            password: "correct horse",
            email: Some(email),
            role_id: role.id,
            status: moekura_db::users::UserStatus::Active,
        },
    )
    .await
    .unwrap();
    let session = moekura_db::sessions::create(
        pool,
        moekura_db::sessions::NewSession {
            user_id: user.id,
            user_agent: None,
            ip: None,
        },
        moekura_db::sessions::Lifetime {
            idle: std::time::Duration::from_secs(3600),
            max: std::time::Duration::from_secs(3600),
        },
    )
    .await
    .unwrap();
    (user, session)
}

/// Gives post `post_id` the perceptual hash of `png`, as processing
/// would if they were the same picture.
pub async fn hash_like(state: &AppState, pool: &PgPool, post_id: i64, png: &[u8]) {
    let dir = state.work_dir.join(format!("hash-like-{post_id}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.png");
    std::fs::write(&path, png).unwrap();
    let hash = state
        .media
        .perceptual_hash(&path, moekura_media::MediaType::Png, &dir)
        .await
        .unwrap();
    let asset = moekura_db::media::for_post(pool, post_id)
        .await
        .unwrap()
        .unwrap();
    moekura_db::media::mark_processed(pool, asset.id, Some(hash))
        .await
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
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

    pub fn gif(width: u32, height: u32) -> Vec<u8> {
        encode(width, height, "gif")
    }

    /// A PNG with a text chunk saying `secret`, as cameras and editors
    /// leave in files.
    pub fn png_with_text(width: u32, height: u32, secret: &str) -> Vec<u8> {
        png_with_chunk(width, height, "Comment", secret)
    }

    /// A PNG with a text chunk `keyword` saying `text` (`parameters` is
    /// where Stable Diffusion front ends put theirs).
    pub fn png_with_chunk(width: u32, height: u32, keyword: &str, text: &str) -> Vec<u8> {
        let plain = png(width, height);
        let data = [keyword.as_bytes(), b"\0", text.as_bytes()].concat();
        let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
        chunk.extend_from_slice(b"tEXt");
        chunk.extend_from_slice(&data);
        // CRC-32 of the type and data.
        let mut crc = 0xFFFF_FFFFu32;
        for byte in b"tEXt".iter().chain(&data) {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        chunk.extend_from_slice(&(!crc).to_be_bytes());
        // After the signature and IHDR.
        [&plain[..33], &chunk, &plain[33..]].concat()
    }
}

/// The requester a session token stands for, for calling handlers'
/// helpers directly.
pub async fn current_user(state: &AppState, session: &str) -> crate::auth::CurrentUser {
    let found = moekura_db::sessions::lookup(state.db.primary(), session)
        .await
        .unwrap()
        .expect("a live session");
    let mut current = crate::auth::CurrentUser::for_user(found.user, found.ban, &state.site.get());
    current.logged_in_at = Some(found.created_at);
    current
}
