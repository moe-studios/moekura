//! Infrastructure configuration.
//!
//! These settings come from `moekura.toml` and `MOEKURA_*` environment
//! variables and require a restart to change. Site settings that admins edit
//! at runtime live in the database instead.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub cache: CacheConfig,
    pub jobs: JobsConfig,
    pub mail: MailConfig,
    pub media: MediaConfig,
    pub paths: PathsConfig,
    pub search: SearchConfig,
    pub storage: StorageConfig,
    pub telemetry: TelemetryConfig,
    pub webhooks: WebhooksConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the HTTP server listens on.
    pub bind: SocketAddr,
    /// The URL users reach the site at. Cookies are marked `Secure` when it
    /// is `https`, and form posts are only accepted from this origin.
    pub public_url: Url,
    /// Reverse proxies whose `X-Forwarded-For` header is believed. Requests
    /// from anywhere else are identified by their connection address, so
    /// clients can't spoof their IP by sending the header themselves.
    pub trusted_proxies: Vec<IpNet>,
    /// Requests running longer than this are aborted with `408`.
    pub request_timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080)),
            public_url: Url::parse("http://localhost:8080").expect("valid default URL"),
            trusted_proxies: Vec::new(),
            request_timeout_secs: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Connection URL of the primary, e.g. `postgres://user:pass@host/db`.
    pub url: String,
    /// Read replicas. Replica-safe reads are spread across these; when empty,
    /// every query goes to the primary.
    pub replicas: Vec<String>,
    /// Maximum connections per pool (the primary and each replica).
    pub max_connections: u32,
    pub min_connections: u32,
    /// How long to wait for a free pooled connection.
    pub acquire_timeout_secs: u64,
    /// Server-side limit for a single statement; `0` disables it.
    pub statement_timeout_ms: u64,
    /// Apply pending migrations when `serve` starts. Large deployments should
    /// turn this off and run `moekura migrate` as a separate release step.
    pub auto_migrate: bool,
    /// Replicas further behind the primary than this are skipped until they
    /// catch up. It is also how long someone's reads stay on the primary
    /// after they change something, so they see their own changes.
    pub replica_max_lag_secs: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            replicas: Vec::new(),
            max_connections: 16,
            min_connections: 0,
            acquire_timeout_secs: 5,
            statement_timeout_ms: 30_000,
            auto_migrate: true,
            replica_max_lag_secs: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// A session ends after this many days without use.
    pub session_idle_days: u32,
    /// A session ends this many days after login, however active.
    pub session_max_days: u32,
    /// Logging in through an OpenID Connect provider (single sign-on).
    pub oidc: Option<OidcConfig>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_idle_days: 30,
            session_max_days: 365,
            oidc: None,
        }
    }
}

/// An OpenID Connect provider people can log in with: Authentik,
/// Keycloak, Kanidm, Google, …
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfig {
    /// The provider's issuer URL; it describes itself at
    /// `/.well-known/openid-configuration` under it.
    pub issuer: Url,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    /// The login button's text.
    #[serde(default = "OidcConfig::default_button_label")]
    pub button_label: String,
    #[serde(default = "OidcConfig::default_scopes")]
    pub scopes: Vec<String>,
}

impl OidcConfig {
    fn default_button_label() -> String {
        "Log in with single sign-on".to_owned()
    }

    fn default_scopes() -> Vec<String> {
        ["openid", "email", "profile"].map(String::from).to_vec()
    }
}

/// Outgoing mail over SMTP, for email verification and password resets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MailConfig {
    /// The SMTP server. Empty turns mail off, along with the features that
    /// need it.
    pub host: String,
    /// Defaults to 587 with STARTTLS, 465 with TLS and 25 without.
    pub port: Option<u16>,
    pub tls: MailTls,
    /// Leave both empty if the server doesn't need a login.
    pub username: String,
    pub password: String,
    /// The sender, as `address@example.com` or `Site name <address@example.com>`.
    pub from: String,
    /// Connecting and sending one message give up after this long.
    pub timeout_secs: u64,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: None,
            tls: MailTls::Starttls,
            username: String::new(),
            password: String::new(),
            from: String::new(),
            timeout_secs: 30,
        }
    }
}

impl MailConfig {
    pub fn is_enabled(&self) -> bool {
        !self.host.is_empty()
    }

    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or(match self.tls {
            MailTls::Starttls => 587,
            MailTls::Tls => 465,
            MailTls::None => 25,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailTls {
    /// Connect in plain text, then upgrade with STARTTLS (required).
    Starttls,
    /// TLS from the start ("SMTPS").
    Tls,
    /// No encryption: only for a relay on the same machine or network.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebhooksConfig {
    /// Let webhooks go to private, loopback and link-local addresses (a
    /// service on the same machine or network). Off, so a webhook can't
    /// be used to reach the server's own network.
    pub allow_private_addresses: bool,
    /// Seconds a delivery may take before it counts as failed.
    pub timeout_secs: u64,
}

impl Default for WebhooksConfig {
    fn default() -> Self {
        Self {
            allow_private_addresses: false,
            timeout_secs: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobsConfig {
    /// Jobs processed concurrently per process. Media processing is
    /// CPU-bound, so around the number of cores is a sensible ceiling.
    pub workers: usize,
    /// Run workers inside `serve` too, so one process is enough for small
    /// sites. Turn off when running separate `moekura worker` processes.
    pub run_in_serve: bool,
    /// A job whose worker stops responding for this long is retried
    /// elsewhere. Running jobs renew it continuously.
    pub lock_timeout_secs: u64,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            run_in_serve: true,
            lock_timeout_secs: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    /// Posts per page unless a search asks for another `limit:`.
    pub per_page: u32,
    /// Highest `limit:` a search may ask for.
    pub max_per_page: u32,
    /// Deepest numbered page. Further pages are reached with "next" links,
    /// which don't get slower with depth.
    pub max_page: u32,
    /// Most tags and filters in one search.
    pub max_terms: usize,
    /// Most tags a wildcard expands to (the most used ones).
    pub wildcard_limit: u32,
    /// Result counts are exact up to this many posts, estimated above.
    pub count_limit: u32,
    /// Counts the database expects to cost more than this (in PostgreSQL's
    /// cost units, roughly pages read) are estimated instead of counted:
    /// searches on filters no index covers would otherwise read every post.
    pub count_cost_limit: u32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            per_page: 40,
            max_per_page: 200,
            max_page: 1000,
            max_terms: 40,
            wildcard_limit: 100,
            count_limit: 10_000,
            count_cost_limit: 25_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    pub backend: StorageBackend,
    /// Directory for the `local` backend.
    pub path: PathBuf,
    /// Where browsers fetch files from, e.g. a CDN in front of the bucket.
    /// When unset, the app serves files itself under `/data/`.
    pub public_base_url: Option<Url>,
    pub s3: S3Config,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackend::Local,
            path: PathBuf::from("data"),
            public_base_url: None,
            s3: S3Config::default(),
        }
    }
}

/// Where state shared between web servers lives: rate limit counters and
/// cached search counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    pub backend: CacheBackend,
    /// For `valkey`: `redis://host:6379`, or `rediss://` for TLS. Valkey,
    /// Redis and compatible servers work.
    pub url: Option<String>,
    /// How long a search's result count is reused. `0` turns the count
    /// cache off.
    pub count_ttl_secs: u64,
    /// Starts every key this site stores in Valkey, so several sites can
    /// share one server.
    pub prefix: String,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: CacheBackend::Memory,
            url: None,
            count_ttl_secs: 30,
            prefix: "moekura".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheBackend {
    /// In each process: fine for one web server; with several, each
    /// counts rate limits separately.
    Memory,
    Valkey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackend {
    Local,
    S3,
}

/// Any S3-compatible store: AWS, MinIO, Garage, Cloudflare R2, Backblaze B2…
/// Empty credentials fall back to the standard `AWS_*` environment variables
/// and instance credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    /// For non-AWS stores, e.g. `http://minio:9000`.
    pub endpoint: Option<Url>,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// `bucket` in the path rather than the host name; most self-hosted
    /// stores need this.
    pub path_style: bool,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            bucket: String::new(),
            region: "us-east-1".to_owned(),
            endpoint: None,
            access_key_id: String::new(),
            secret_access_key: String::new(),
            path_style: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    pub max_upload_mb: u64,
    /// Larger images are refused before they are decoded (decompression
    /// bombs).
    pub max_pixels: u64,
    pub max_duration_secs: u64,
    /// Accepted types: jpeg, png, gif, webp, avif, jxl, mp4, webm. `jxl` is
    /// off by default because libvips considers its JPEG XL decoder less
    /// hardened against malicious files.
    pub allowed_types: Vec<String>,
    /// Bounding boxes for thumbnails, e.g. 1x and 2x for high-DPI screens.
    pub thumbnail_sizes: Vec<u32>,
    /// Images larger than this (longest side) also get a resized sample
    /// that the post page shows instead of the original.
    pub sample_size: u32,
    /// `webp` or `avif`.
    pub variant_format: String,
    /// Kill media tools that run longer than this.
    pub tool_timeout_secs: u64,
    /// Scratch space for uploads and processing. Defaults to the system
    /// temporary directory.
    pub work_dir: Option<PathBuf>,
    pub tools: MediaTools,
}

impl MediaConfig {
    /// `work_dir`, or a directory under the system temp dir.
    pub fn work_dir_or_default(&self) -> PathBuf {
        self.work_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("moekura"))
    }
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            max_upload_mb: 100,
            max_pixels: 200_000_000,
            max_duration_secs: 600,
            allowed_types: ["jpeg", "png", "gif", "webp", "avif", "mp4", "webm"]
                .map(String::from)
                .to_vec(),
            thumbnail_sizes: vec![250, 500],
            sample_size: 1600,
            variant_format: "webp".to_owned(),
            tool_timeout_secs: 120,
            work_dir: None,
            tools: MediaTools::default(),
        }
    }
}

/// Paths to the external programs used for media, if not on `PATH`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaTools {
    pub vips: PathBuf,
    pub vipsheader: PathBuf,
    pub vipsthumbnail: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Default for MediaTools {
    fn default() -> Self {
        Self {
            vips: "vips".into(),
            vipsheader: "vipsheader".into(),
            vipsthumbnail: "vipsthumbnail".into(),
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        }
    }
}

/// Directories whose files replace the built-in ones with the same relative
/// path, for theming without recompiling.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathsConfig {
    pub templates_override: Option<PathBuf>,
    pub static_override: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    pub log_format: LogFormat,
    /// `tracing` filter directive; `RUST_LOG` takes precedence when set.
    pub log_filter: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            log_format: LogFormat::Text,
            // Postgres notices like "relation already exists, skipping" are noise.
            log_filter: "info,sqlx::postgres::notice=warn".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Text,
    Json,
}

/// A single problem found by [`Config::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProblem {
    pub key: &'static str,
    pub message: String,
}

impl fmt::Display for ConfigProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

impl Config {
    /// Checks invariants that serde cannot express. Returns every problem
    /// found rather than stopping at the first.
    pub fn validate(&self) -> Result<(), Vec<ConfigProblem>> {
        let mut problems = Vec::new();
        let db = &self.database;

        if db.url.is_empty() {
            problems.push(ConfigProblem {
                key: "database.url",
                message: "is required (set it in the config file or MOEKURA_DATABASE__URL)".into(),
            });
        } else if let Err(message) = check_postgres_url(&db.url) {
            problems.push(ConfigProblem {
                key: "database.url",
                message,
            });
        }
        for (i, replica) in db.replicas.iter().enumerate() {
            if let Err(message) = check_postgres_url(replica) {
                problems.push(ConfigProblem {
                    key: "database.replicas",
                    message: format!("entry {i}: {message}"),
                });
            }
        }
        if self.cache.backend == CacheBackend::Valkey {
            match &self.cache.url {
                None => problems.push(ConfigProblem {
                    key: "cache.url",
                    message: "is required for the valkey backend".into(),
                }),
                Some(url) => {
                    let scheme = Url::parse(url).map(|u| u.scheme().to_owned());
                    if !matches!(scheme.as_deref(), Ok("redis" | "rediss")) {
                        problems.push(ConfigProblem {
                            key: "cache.url",
                            message: "must be a redis:// or rediss:// URL".into(),
                        });
                    }
                }
            }
        }
        if db.max_connections == 0 {
            problems.push(ConfigProblem {
                key: "database.max_connections",
                message: "must be at least 1".into(),
            });
        }
        if db.min_connections > db.max_connections {
            problems.push(ConfigProblem {
                key: "database.min_connections",
                message: "must not exceed database.max_connections".into(),
            });
        }
        if !matches!(self.server.public_url.scheme(), "http" | "https") {
            problems.push(ConfigProblem {
                key: "server.public_url",
                message: "must be an http:// or https:// URL".into(),
            });
        }
        if self.auth.session_idle_days == 0 {
            problems.push(ConfigProblem {
                key: "auth.session_idle_days",
                message: "must be at least 1".into(),
            });
        }
        if self.auth.session_max_days < self.auth.session_idle_days {
            problems.push(ConfigProblem {
                key: "auth.session_max_days",
                message: "must not be less than auth.session_idle_days".into(),
            });
        }
        for (key, dir) in [
            ("paths.templates_override", &self.paths.templates_override),
            ("paths.static_override", &self.paths.static_override),
        ] {
            if let Some(dir) = dir.as_ref().filter(|d| !d.is_dir()) {
                problems.push(ConfigProblem {
                    key,
                    message: format!("{} is not a directory", dir.display()),
                });
            }
        }
        if self.storage.backend == StorageBackend::S3 && self.storage.s3.bucket.is_empty() {
            problems.push(ConfigProblem {
                key: "storage.s3.bucket",
                message: "is required when storage.backend is s3".into(),
            });
        }
        if let Some(url) = &self.storage.public_base_url
            && !matches!(url.scheme(), "http" | "https")
        {
            problems.push(ConfigProblem {
                key: "storage.public_base_url",
                message: "must be an http:// or https:// URL".into(),
            });
        }
        const MEDIA_TYPES: &[&str] = crate::search::FILETYPES;
        if let Some(unknown) = self
            .media
            .allowed_types
            .iter()
            .find(|t| !MEDIA_TYPES.contains(&t.as_str()))
        {
            problems.push(ConfigProblem {
                key: "media.allowed_types",
                message: format!(
                    "unknown type `{unknown}` (known: {})",
                    MEDIA_TYPES.join(", ")
                ),
            });
        }
        let search = &self.search;
        for (key, value) in [
            ("search.per_page", search.per_page),
            ("search.max_page", search.max_page),
            ("search.wildcard_limit", search.wildcard_limit),
            ("search.count_limit", search.count_limit),
        ] {
            if value == 0 {
                problems.push(ConfigProblem {
                    key,
                    message: "must be at least 1".into(),
                });
            }
        }
        if search.max_per_page < search.per_page {
            problems.push(ConfigProblem {
                key: "search.max_per_page",
                message: "must not be less than search.per_page".into(),
            });
        }
        if search.max_terms == 0 {
            problems.push(ConfigProblem {
                key: "search.max_terms",
                message: "must be at least 1".into(),
            });
        }
        if !matches!(self.media.variant_format.as_str(), "webp" | "avif") {
            problems.push(ConfigProblem {
                key: "media.variant_format",
                message: "must be webp or avif".into(),
            });
        }
        if self.media.thumbnail_sizes.is_empty() || self.media.thumbnail_sizes.contains(&0) {
            problems.push(ConfigProblem {
                key: "media.thumbnail_sizes",
                message: "needs at least one size, all above 0".into(),
            });
        }
        if self.media.max_upload_mb == 0 {
            problems.push(ConfigProblem {
                key: "media.max_upload_mb",
                message: "must be at least 1".into(),
            });
        }
        if self.jobs.workers == 0 {
            problems.push(ConfigProblem {
                key: "jobs.workers",
                message: "must be at least 1".into(),
            });
        }
        if self.jobs.lock_timeout_secs < 10 {
            problems.push(ConfigProblem {
                key: "jobs.lock_timeout_secs",
                message: "must be at least 10".into(),
            });
        }
        if let Some(oidc) = &self.auth.oidc {
            let loopback = oidc.issuer.host_str().is_some_and(|h| {
                h == "localhost"
                    || h.parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
            if !(oidc.issuer.scheme() == "https" || (oidc.issuer.scheme() == "http" && loopback)) {
                problems.push(ConfigProblem {
                    key: "auth.oidc.issuer",
                    message: "must be an https:// URL".into(),
                });
            }
            if oidc.client_id.is_empty() {
                problems.push(ConfigProblem {
                    key: "auth.oidc.client_id",
                    message: "is required".into(),
                });
            }
            if !oidc.scopes.iter().any(|s| s == "openid") {
                problems.push(ConfigProblem {
                    key: "auth.oidc.scopes",
                    message: "must include `openid`".into(),
                });
            }
        }
        let mail = &self.mail;
        if mail.is_enabled() {
            if !mail.from.contains('@') {
                problems.push(ConfigProblem {
                    key: "mail.from",
                    message: "is required when mail.host is set, as `address@example.com` or \
                              `Site name <address@example.com>`"
                        .into(),
                });
            }
            if mail.username.is_empty() != mail.password.is_empty() {
                problems.push(ConfigProblem {
                    key: "mail.username",
                    message: "set both mail.username and mail.password, or neither".into(),
                });
            }
            if mail.port == Some(0) {
                problems.push(ConfigProblem {
                    key: "mail.port",
                    message: "must be a port number".into(),
                });
            }
            if mail.timeout_secs == 0 {
                problems.push(ConfigProblem {
                    key: "mail.timeout_secs",
                    message: "must be at least 1".into(),
                });
            }
        }
        if self.server.request_timeout_secs == 0 {
            problems.push(ConfigProblem {
                key: "server.request_timeout_secs",
                message: "must be at least 1".into(),
            });
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    /// A copy with credentials masked, safe to print or log.
    pub fn redacted(&self) -> Self {
        let mut config = self.clone();
        config.database.url = redact_url(&config.database.url);
        for replica in &mut config.database.replicas {
            *replica = redact_url(replica);
        }
        if let Some(url) = &mut config.cache.url {
            *url = redact_url(url);
        }
        if !config.storage.s3.secret_access_key.is_empty() {
            config.storage.s3.secret_access_key = REDACTED.to_owned();
        }
        if let Some(oidc) = &mut config.auth.oidc
            && !oidc.client_secret.is_empty()
        {
            oidc.client_secret = REDACTED.to_owned();
        }
        if !config.mail.password.is_empty() {
            config.mail.password = REDACTED.to_owned();
        }
        config
    }
}

fn check_postgres_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|e| format!("not a valid URL ({e})"))?;
    match url.scheme() {
        "postgres" | "postgresql" => Ok(()),
        other => Err(format!(
            "unsupported scheme `{other}`, expected `postgres://`"
        )),
    }
}

const REDACTED: &str = "REDACTED";

/// Masks the password in the userinfo part and in a `password` query
/// parameter. Anything that is not a well-formed `postgres://` URL is
/// replaced entirely: it parses in ways we cannot predict and may still
/// contain a secret.
pub fn redact_url(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let mut url = match Url::parse(raw) {
        Ok(url) if matches!(url.scheme(), "postgres" | "postgresql" | "redis" | "rediss") => url,
        _ => return format!("<not a database URL, {REDACTED}>"),
    };
    if url.password().is_some() {
        // Only fails for URLs that cannot have credentials, which we just
        // established this one does.
        let _ = url.set_password(Some(REDACTED));
    }
    if url.query_pairs().any(|(k, _)| k == "password") {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| {
                let v = if k == "password" {
                    REDACTED.into()
                } else {
                    v.into_owned()
                };
                (k.into_owned(), v)
            })
            .collect();
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    url.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Config {
        let mut config = Config::default();
        config.database.url = "postgres://moekura:hunter2@localhost/moekura".into();
        config
    }

    #[test]
    fn default_config_requires_database_url() {
        let problems = Config::default().validate().unwrap_err();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].key, "database.url");
    }

    #[test]
    fn valid_config_passes() {
        valid().validate().unwrap();
    }

    #[test]
    fn collects_every_problem() {
        let mut config = valid();
        config.database.url = "mysql://localhost/moekura".into();
        config.database.replicas = vec!["not a url".into()];
        config.database.max_connections = 0;
        let keys: Vec<_> = config
            .validate()
            .unwrap_err()
            .iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(
            keys,
            [
                "database.url",
                "database.replicas",
                "database.max_connections"
            ]
        );
    }

    #[test]
    fn checks_public_url_and_session_lengths() {
        let mut config = valid();
        config.server.public_url = Url::parse("ftp://example.com").unwrap();
        config.auth.session_idle_days = 10;
        config.auth.session_max_days = 5;
        let keys: Vec<_> = config
            .validate()
            .unwrap_err()
            .iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(keys, ["server.public_url", "auth.session_max_days"]);
    }

    #[test]
    fn min_connections_cannot_exceed_max() {
        let mut config = valid();
        config.database.min_connections = 20;
        config.database.max_connections = 10;
        let problems = config.validate().unwrap_err();
        assert_eq!(problems[0].key, "database.min_connections");
    }

    #[test]
    fn redacts_userinfo_password() {
        assert_eq!(
            redact_url("postgres://moekura:hunter2@db:5432/moekura"),
            "postgres://moekura:REDACTED@db:5432/moekura"
        );
    }

    #[test]
    fn redacts_query_password() {
        assert_eq!(
            redact_url("postgres://db/moekura?user=moekura&password=hunter2&sslmode=require"),
            "postgres://db/moekura?user=moekura&password=REDACTED&sslmode=require"
        );
    }

    #[test]
    fn leaves_passwordless_urls_alone() {
        assert_eq!(
            redact_url("postgres://moekura@db/moekura"),
            "postgres://moekura@db/moekura"
        );
        assert_eq!(redact_url(""), "");
    }

    #[test]
    fn replaces_non_postgres_urls() {
        // Parses as a URL with scheme `moekura`, so only the scheme check stops the leak.
        assert!(!redact_url("moekura:hunter2 garbage").contains("hunter2"));
        assert!(!redact_url("not even a url hunter2").contains("hunter2"));
        assert!(!redact_url("mysql://u:hunter2@db/x").contains("hunter2"));
    }

    #[test]
    fn s3_needs_a_bucket_and_its_secret_is_redacted() {
        let mut config = valid();
        config.storage.backend = StorageBackend::S3;
        let problems = config.validate().unwrap_err();
        assert_eq!(problems[0].key, "storage.s3.bucket");

        config.storage.s3.bucket = "posts".into();
        config.storage.s3.secret_access_key = "very secret".into();
        config.validate().unwrap();
        assert_eq!(config.redacted().storage.s3.secret_access_key, "REDACTED");
    }

    #[test]
    fn checks_the_cache_backend() {
        let mut config = valid();
        config.cache.backend = CacheBackend::Valkey;
        let problems = config.validate().unwrap_err();
        assert_eq!(problems[0].key, "cache.url");
        config.cache.url = Some("http://valkey:6379".into());
        assert_eq!(config.validate().unwrap_err()[0].key, "cache.url");
        config.cache.url = Some("redis://:secret@valkey:6379".into());
        assert!(config.validate().is_ok());
        assert!(!config.redacted().cache.url.unwrap().contains("secret"));
    }

    #[test]
    fn checks_mail_and_redacts_its_password() {
        let mut config = valid();
        config.mail.password = "ignored".into();
        config.validate().unwrap();

        config.mail.host = "smtp.example.com".into();
        config.mail.timeout_secs = 0;
        let keys: Vec<_> = config
            .validate()
            .unwrap_err()
            .iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(keys, ["mail.from", "mail.username", "mail.timeout_secs"]);

        config.mail.from = "Moekura <noreply@example.com>".into();
        config.mail.username = "moekura".into();
        config.mail.timeout_secs = 30;
        config.validate().unwrap();
        assert_eq!(config.redacted().mail.password, "REDACTED");
        assert_eq!(config.mail.port_or_default(), 587);
        config.mail.tls = MailTls::Tls;
        assert_eq!(config.mail.port_or_default(), 465);
    }

    #[test]
    fn checks_oidc_and_redacts_its_secret() {
        let parsed: Config = toml::from_str(
            "[auth.oidc]\nissuer = \"https://sso.example.com/realms/booru\"\nclient_id = \"moekura\"\nclient_secret = \"hush\"\n",
        )
        .unwrap();
        let oidc = parsed.auth.oidc.clone().unwrap();
        assert_eq!(oidc.scopes, ["openid", "email", "profile"]);
        assert_eq!(oidc.button_label, "Log in with single sign-on");
        let mut config = valid();
        config.auth.oidc = Some(oidc);
        config.validate().unwrap();
        assert_eq!(
            config.redacted().auth.oidc.unwrap().client_secret,
            "REDACTED"
        );

        let oidc = config.auth.oidc.as_mut().unwrap();
        oidc.issuer = Url::parse("http://sso.example.com").unwrap();
        oidc.client_id = String::new();
        oidc.scopes = vec!["email".into()];
        let keys: Vec<_> = config
            .validate()
            .unwrap_err()
            .iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(
            keys,
            [
                "auth.oidc.issuer",
                "auth.oidc.client_id",
                "auth.oidc.scopes"
            ]
        );
        // Plain HTTP is fine for a provider on the same machine.
        let oidc = config.auth.oidc.as_mut().unwrap();
        oidc.issuer = Url::parse("http://127.0.0.1:9000").unwrap();
        oidc.client_id = "moekura".into();
        oidc.scopes = vec!["openid".into()];
        config.validate().unwrap();
    }

    #[test]
    fn redacted_masks_replicas() {
        let mut config = valid();
        config.database.replicas = vec!["postgres://r:secret@replica/moekura".into()];
        let redacted = config.redacted();
        assert!(!redacted.database.url.contains("hunter2"));
        assert!(!redacted.database.replicas[0].contains("secret"));
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = toml::from_str::<Config>("[database]\nurll = \"x\"\n").unwrap_err();
        assert!(err.to_string().contains("urll"), "{err}");
    }

    #[test]
    fn round_trips_through_toml() {
        let config = valid();
        let text = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }
}
