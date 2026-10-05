//! Infrastructure configuration.
//!
//! These settings come from `moekura.toml` and `MOEKURA_*` environment
//! variables and require a restart to change. Site settings that admins edit
//! at runtime live in the database instead.

use std::collections::BTreeMap;
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
    pub sources: SourcesConfig,
    pub storage: StorageConfig,
    pub tagger: TaggerConfig,
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
    /// Requests a minute each API client may make (per account, or per
    /// address for visitors) to `/api/v1` and the Danbooru-compatible
    /// API, on average. 0: no limit.
    pub api_requests_per_minute: u32,
    /// How many of those may come at once, before the per-minute rate
    /// applies.
    pub api_burst: u32,
    /// Which other websites' scripts may call the APIs.
    pub cors: CorsConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080)),
            public_url: Url::parse("http://localhost:8080").expect("valid default URL"),
            trusted_proxies: Vec::new(),
            request_timeout_secs: 30,
            api_requests_per_minute: 300,
            api_burst: 60,
            cors: CorsConfig::default(),
        }
    }
}

/// Cross-origin access to `/api/v1` and the Danbooru-compatible API from
/// scripts on other websites. Nothing is allowed by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorsConfig {
    /// Origins (`https://app.example.com`) whose scripts may call the
    /// APIs, or `"*"` for any website.
    pub allowed_origins: Vec<String>,
    /// Whether scripts on the listed origins may send the visitor's
    /// session cookie. Without it, cross-origin requests authenticate with
    /// an API key only. Needs explicit origins, not `"*"`.
    pub allow_credentials: bool,
    /// How long browsers may cache a preflight answer, in seconds.
    pub max_age_secs: u64,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            allow_credentials: false,
            max_age_secs: 600,
        }
    }
}

impl CorsConfig {
    /// Whether any website is allowed.
    pub fn allows_any(&self) -> bool {
        self.allowed_origins.iter().any(|o| o == "*")
    }
}

/// Why `origin` isn't a bare `scheme://host[:port]` origin.
fn check_origin(origin: &str) -> Result<(), String> {
    let url = Url::parse(origin).map_err(|e| format!("`{origin}` is not a URL ({e})"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("`{origin}` must be an http:// or https:// origin"));
    }
    let serialized = url.origin().ascii_serialization();
    if serialized != origin {
        return Err(format!(
            "`{origin}` must be just an origin, written `{serialized}`"
        ));
    }
    Ok(())
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
    /// A captcha service; site settings say where it's asked for.
    pub captcha: Option<CaptchaConfig>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_idle_days: 30,
            session_max_days: 365,
            oidc: None,
            captcha: None,
        }
    }
}

/// A captcha service that checks tokens its widget hands the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptchaConfig {
    pub provider: CaptchaProvider,
    /// The public key the widget shows.
    pub site_key: String,
    /// The private key tokens are checked with.
    pub secret_key: String,
    /// Where tokens are checked; the provider's own address by default.
    #[serde(default)]
    pub verify_url: Option<Url>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptchaProvider {
    /// Cloudflare Turnstile.
    Turnstile,
    Hcaptcha,
}

impl CaptchaProvider {
    /// The script that draws the widget.
    pub fn script_url(self) -> &'static str {
        match self {
            CaptchaProvider::Turnstile => "https://challenges.cloudflare.com/turnstile/v0/api.js",
            CaptchaProvider::Hcaptcha => "https://js.hcaptcha.com/1/api.js",
        }
    }

    /// The origins the widget loads scripts, frames and styles from, for
    /// the content security policy.
    pub fn origins(self) -> &'static [&'static str] {
        match self {
            CaptchaProvider::Turnstile => &["https://challenges.cloudflare.com"],
            CaptchaProvider::Hcaptcha => &["https://hcaptcha.com", "https://*.hcaptcha.com"],
        }
    }

    /// The element class the script turns into a widget.
    pub fn widget_class(self) -> &'static str {
        match self {
            CaptchaProvider::Turnstile => "cf-turnstile",
            CaptchaProvider::Hcaptcha => "h-captcha",
        }
    }

    /// The form field the widget puts its token in.
    pub fn response_field(self) -> &'static str {
        match self {
            CaptchaProvider::Turnstile => "cf-turnstile-response",
            CaptchaProvider::Hcaptcha => "h-captcha-response",
        }
    }

    /// Where tokens are checked.
    pub fn verify_url(self) -> &'static str {
        match self {
            CaptchaProvider::Turnstile => {
                "https://challenges.cloudflare.com/turnstile/v0/siteverify"
            }
            CaptchaProvider::Hcaptcha => "https://api.hcaptcha.com/siteverify",
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

/// Reading where uploads come from (the source strategies).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourcesConfig {
    /// Logins for sites that only show works (or all of them) to
    /// members, by domain: `[sources.logins."gelbooru.com"]`. Requests to
    /// that domain and its subdomains carry them.
    pub logins: BTreeMap<String, SiteLogin>,
    /// How posts on X are read.
    pub x: XSourceConfig,
}

/// How posts on X are read: through an FxEmbed instance's API, then, if
/// `[sources.logins."x.com"]` has a login, from X as that account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct XSourceConfig {
    /// The API of an FxEmbed instance (fxtwitter, fixupx or your own),
    /// which reads posts, age-restricted ones too, without an account
    /// here. Empty: don't use one.
    pub fxembed_api_url: String,
}

impl Default for XSourceConfig {
    fn default() -> Self {
        Self {
            fxembed_api_url: "https://api.fixupx.com".into(),
        }
    }
}

/// What requests to a site carry to be logged in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SiteLogin {
    /// The `Cookie` header: `"PHPSESSID=…"`, `"a=…; b=…"`.
    pub cookie: String,
    /// Added to the address's query (Gelbooru's `user_id` and `api_key`).
    pub query: BTreeMap<String, String>,
    /// Other headers (`Authorization = "Bearer …"`).
    pub headers: BTreeMap<String, String>,
}

impl SourcesConfig {
    /// The login for `host`: its domain's, or a parent domain's.
    pub fn login_for(&self, host: &str) -> Option<&SiteLogin> {
        let host = host.to_ascii_lowercase();
        self.logins.iter().find_map(|(domain, login)| {
            let domain = domain.to_ascii_lowercase();
            (host == domain || host.ends_with(&format!(".{domain}"))).then_some(login)
        })
    }
}

/// The optional tagger (`moekura tagger`), which suggests tags for new
/// uploads with a machine learning model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaggerConfig {
    /// Queue new uploads for the tagger. Set it for every process: those
    /// processing uploads queue the work, and `moekura tagger` does it.
    pub enabled: bool,
    /// A model Moekura knows by name (see the book), or `custom` for one
    /// given by the four settings below.
    pub model: String,
    /// For `custom`: an ONNX WD-tagger-style model and its
    /// `selected_tags.csv`, with their SHA-256 checksums.
    pub model_url: Option<Url>,
    pub model_sha256: String,
    pub tags_url: Option<Url>,
    pub tags_sha256: String,
    /// Where models are downloaded to, one directory each.
    pub model_dir: PathBuf,
    /// The ONNX Runtime library (`libonnxruntime.so`). When unset,
    /// `ORT_DYLIB_PATH`, or the system's library path.
    pub runtime: Option<PathBuf>,
    /// Threads one image uses; `0` means one per core.
    pub threads: usize,
    /// Posts tagged at once. Each runs the whole model, so one is usually
    /// best: raise `threads` instead.
    pub workers: usize,
    /// The account tags applied automatically are credited to. Created on
    /// first use, without a password; an existing account that has one
    /// isn't used.
    pub account: String,
}

impl Default for TaggerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: crate::tagger::DEFAULT_MODEL.to_owned(),
            model_url: None,
            model_sha256: String::new(),
            tags_url: None,
            tags_sha256: String::new(),
            model_dir: PathBuf::from("data/models"),
            runtime: None,
            threads: 0,
            workers: 1,
            account: "tagger".to_owned(),
        }
    }
}

/// Where a model's files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSource {
    pub name: String,
    pub model_url: Url,
    pub model_sha256: String,
    pub tags_url: Url,
    pub tags_sha256: String,
}

impl TaggerConfig {
    /// The configured model's files; `None` when the configuration is
    /// incomplete (which [`Config::validate`] reports).
    pub fn source(&self) -> Option<ModelSource> {
        if self.model == "custom" {
            return Some(ModelSource {
                name: self.model.clone(),
                model_url: self.model_url.clone()?,
                model_sha256: self.model_sha256.to_ascii_lowercase(),
                tags_url: self.tags_url.clone()?,
                tags_sha256: self.tags_sha256.to_ascii_lowercase(),
            });
        }
        let preset = crate::tagger::preset(&self.model)?;
        Some(ModelSource {
            name: preset.name.to_owned(),
            model_url: Url::parse(preset.model_url).ok()?,
            model_sha256: preset.model_sha256.to_owned(),
            tags_url: Url::parse(preset.tags_url).ok()?,
            tags_sha256: preset.tags_sha256.to_owned(),
        })
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
    /// Accepted types: jpeg, png, gif, webp, avif, jxl, mp4, webm, ugoira
    /// (Pixiv's zips of frames). `jxl` is off by default because libvips
    /// considers its JPEG XL decoder less hardened against malicious files.
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
    /// Whether identifying metadata (EXIF, GPS, XMP, IPTC, comments) is
    /// removed from uploaded originals, not just from thumbnails. On by
    /// default: anyone can download an original, and the metadata page
    /// hiding GPS and serial numbers would otherwise suggest they're gone.
    pub strip_metadata: StripMetadata,
    /// Media tool processes running at once in this process, for uploads
    /// and jobs together; more wait their turn. 0: one per CPU core.
    pub max_tool_processes: u32,
    /// Threads each ffmpeg run may use for decoding, filtering and
    /// encoding. 0: ffmpeg's choice (about one per core).
    pub ffmpeg_threads: u32,
    /// Memory (address space, including its libraries and thread stacks)
    /// each ffmpeg or ffprobe run may use, in MB. 0: no limit. Linux only.
    pub ffmpeg_memory_mb: u64,
    /// CPU time each ffmpeg or ffprobe run may use, in seconds, all its
    /// threads together. 0: no limit (`tool_timeout_secs` and
    /// `ffmpeg_threads` still bound it). Linux only.
    pub ffmpeg_cpu_secs: u64,
    /// Scratch space for uploads and processing. Defaults to the system
    /// temporary directory.
    pub work_dir: Option<PathBuf>,
    pub tools: MediaTools,
}

/// The least `media.ffmpeg_memory_mb` ffmpeg starts with (its libraries
/// alone take a few hundred MB of address space).
pub const MIN_FFMPEG_MEMORY_MB: u64 = 512;

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
            allowed_types: [
                "jpeg", "png", "gif", "webp", "avif", "mp4", "webm", "ugoira",
            ]
            .map(String::from)
            .to_vec(),
            thumbnail_sizes: vec![250, 500],
            sample_size: 1600,
            variant_format: "webp".to_owned(),
            tool_timeout_secs: 120,
            strip_metadata: StripMetadata::Strip,
            max_tool_processes: 0,
            ffmpeg_threads: 2,
            ffmpeg_memory_mb: 2048,
            ffmpeg_cpu_secs: 0,
            work_dir: None,
            tools: MediaTools::default(),
        }
    }
}

/// What happens to the metadata in uploaded originals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StripMetadata {
    /// Originals are kept exactly as uploaded.
    Off,
    /// Removed from the types that support it (JPEG, PNG, WebP); others
    /// are kept as uploaded.
    #[default]
    Strip,
    /// Removed, and files of other types refused.
    Require,
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
    /// Translations (`<language>/*.ftl`) that add languages or replace
    /// built-in messages.
    pub locales_override: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    pub log_format: LogFormat,
    /// `tracing` filter directive; `RUST_LOG` takes precedence when set.
    pub log_filter: String,
    /// Where `serve` and `worker` each serve Prometheus metrics, at
    /// `/metrics` (e.g. `127.0.0.1:9100`). A separate listener, never the
    /// site's. Unset: no metrics, at no cost.
    pub metrics_bind: Option<SocketAddr>,
    /// An OpenTelemetry collector's OTLP/HTTP address (e.g.
    /// `http://otel-collector:4318`); traces of requests and jobs are sent
    /// to its `/v1/traces`. Unset: no traces.
    pub otlp_endpoint: Option<Url>,
    /// The share of requests and jobs traced, from 0.0 to 1.0.
    pub otlp_sample_ratio: f64,
    /// `service.name` in exported traces.
    pub service_name: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            log_format: LogFormat::Text,
            // Postgres notices like "relation already exists, skipping" are
            // noise, as is ONNX Runtime narrating the tagger's model setup.
            log_filter: "info,sqlx::postgres::notice=warn,ort=warn".to_owned(),
            metrics_bind: None,
            otlp_endpoint: None,
            otlp_sample_ratio: 1.0,
            service_name: "moekura".to_owned(),
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
        if self.server.api_requests_per_minute > 0 && self.server.api_burst == 0 {
            problems.push(ConfigProblem {
                key: "server.api_burst",
                message: "must be at least 1 while api_requests_per_minute is set".into(),
            });
        }
        let telemetry = &self.telemetry;
        if let Some(url) = &telemetry.otlp_endpoint
            && !matches!(url.scheme(), "http" | "https")
        {
            problems.push(ConfigProblem {
                key: "telemetry.otlp_endpoint",
                message: "must be an http:// or https:// URL".into(),
            });
        }
        if !(0.0..=1.0).contains(&telemetry.otlp_sample_ratio) {
            problems.push(ConfigProblem {
                key: "telemetry.otlp_sample_ratio",
                message: "must be from 0.0 to 1.0".into(),
            });
        }
        if telemetry.metrics_bind.is_some() && telemetry.metrics_bind == Some(self.server.bind) {
            problems.push(ConfigProblem {
                key: "telemetry.metrics_bind",
                message: "must differ from server.bind; metrics have their own listener".into(),
            });
        }
        let cors = &self.server.cors;
        for origin in cors.allowed_origins.iter().filter(|o| *o != "*") {
            if let Err(message) = check_origin(origin) {
                problems.push(ConfigProblem {
                    key: "server.cors.allowed_origins",
                    message,
                });
            }
        }
        if cors.allow_credentials && cors.allows_any() {
            problems.push(ConfigProblem {
                key: "server.cors.allow_credentials",
                message: "needs explicit allowed_origins, not \"*\"".into(),
            });
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
            ("paths.locales_override", &self.paths.locales_override),
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
        let fxembed_api_url = self.sources.x.fxembed_api_url.trim();
        if !fxembed_api_url.is_empty()
            && !Url::parse(fxembed_api_url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
        {
            problems.push(ConfigProblem {
                key: "sources.x.fxembed_api_url",
                message: "must be an http:// or https:// URL, or empty".into(),
            });
        }
        if (1..MIN_FFMPEG_MEMORY_MB).contains(&self.media.ffmpeg_memory_mb) {
            problems.push(ConfigProblem {
                key: "media.ffmpeg_memory_mb",
                message: format!(
                    "must be 0 (no limit) or at least {MIN_FFMPEG_MEMORY_MB}: ffmpeg needs that much to start"
                ),
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
        if let Some(captcha) = &self.auth.captcha {
            for (key, value) in [
                ("auth.captcha.site_key", &captcha.site_key),
                ("auth.captcha.secret_key", &captcha.secret_key),
            ] {
                if value.trim().is_empty() {
                    problems.push(ConfigProblem {
                        key,
                        message: "is required".into(),
                    });
                }
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
        let tagger = &self.tagger;
        if tagger.model == "custom" {
            for (key, url) in [
                ("tagger.model_url", &tagger.model_url),
                ("tagger.tags_url", &tagger.tags_url),
            ] {
                match url {
                    None => problems.push(ConfigProblem {
                        key,
                        message: "is required when tagger.model is custom".into(),
                    }),
                    Some(url) if !matches!(url.scheme(), "http" | "https") => {
                        problems.push(ConfigProblem {
                            key,
                            message: "must be an http:// or https:// URL".into(),
                        });
                    }
                    Some(_) => {}
                }
            }
            for (key, sum) in [
                ("tagger.model_sha256", &tagger.model_sha256),
                ("tagger.tags_sha256", &tagger.tags_sha256),
            ] {
                if sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit()) {
                    problems.push(ConfigProblem {
                        key,
                        message: "must be a SHA-256 checksum (64 hexadecimal digits)".into(),
                    });
                }
            }
        } else if crate::tagger::preset(&tagger.model).is_none() {
            let known: Vec<&str> = crate::tagger::PRESETS.iter().map(|p| p.name).collect();
            problems.push(ConfigProblem {
                key: "tagger.model",
                message: format!(
                    "unknown model `{}` (known: {}, or custom)",
                    tagger.model,
                    known.join(", ")
                ),
            });
        }
        if tagger.workers == 0 {
            problems.push(ConfigProblem {
                key: "tagger.workers",
                message: "must be at least 1".into(),
            });
        }
        if crate::accounts::UserName::parse(&tagger.account).is_err() {
            problems.push(ConfigProblem {
                key: "tagger.account",
                message: "must be a valid user name".into(),
            });
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

    /// A copy with credentials masked, safe to print or log: passwords,
    /// secret keys, site logins, and whatever an address carries in its
    /// userinfo or query.
    pub fn redacted(&self) -> Self {
        let mut config = self.clone();
        config.database.url = redact_url(&config.database.url);
        for replica in &mut config.database.replicas {
            *replica = redact_url(replica);
        }
        if let Some(url) = &mut config.cache.url {
            *url = redact_url(url);
        }
        redact_secret(&mut config.storage.s3.secret_access_key);
        if let Some(oidc) = &mut config.auth.oidc {
            redact_secret(&mut oidc.client_secret);
        }
        if let Some(captcha) = &mut config.auth.captcha {
            redact_secret(&mut captcha.secret_key);
        }
        redact_secret(&mut config.mail.password);
        // Session cookies, API keys and tokens for other sites: names are
        // kept, so it still shows what is sent where.
        for login in config.sources.logins.values_mut() {
            redact_secret(&mut login.cookie);
            login.query.values_mut().for_each(redact_secret);
            login.headers.values_mut().for_each(redact_secret);
        }
        if let Ok(mut url) = Url::parse(&config.sources.x.fxembed_api_url)
            && redact_web_url(&mut url)
        {
            config.sources.x.fxembed_api_url = url.into();
        }
        for url in [
            config.telemetry.otlp_endpoint.as_mut(),
            config.tagger.model_url.as_mut(),
            config.tagger.tags_url.as_mut(),
            config.storage.public_base_url.as_mut(),
            config.storage.s3.endpoint.as_mut(),
            config
                .auth
                .captcha
                .as_mut()
                .and_then(|c| c.verify_url.as_mut()),
        ]
        .into_iter()
        .flatten()
        {
            redact_web_url(url);
        }
        config
    }
}

/// Masks a secret that is set, leaving an unset one visibly empty.
fn redact_secret(secret: &mut String) {
    if !secret.is_empty() {
        *secret = REDACTED.to_owned();
    }
}

/// Masks the userinfo and every query value of a web address, where
/// services take credentials and API keys. Says whether there were any.
fn redact_web_url(url: &mut Url) -> bool {
    let secret = !url.username().is_empty() || url.password().is_some() || url.query().is_some();
    // Both only fail for URLs that cannot have credentials, which then
    // have none to hide.
    if !url.username().is_empty() {
        let _ = url.set_username(REDACTED);
    }
    if url.password().is_some() {
        let _ = url.set_password(Some(REDACTED));
    }
    if url.query().is_some() {
        let keys: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(keys.iter().map(|k| (k, REDACTED)));
    }
    secret
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
    fn site_logins_cover_subdomains() {
        let config: Config = toml::from_str(
            r#"
            [sources.logins."gelbooru.com"]
            query = { user_id = "7", api_key = "k" }
            [sources.logins."pawoo.net"]
            headers = { Authorization = "Bearer t" }
            "#,
        )
        .unwrap();
        let login = config.sources.login_for("img2.gelbooru.com").unwrap();
        assert_eq!(login.query["api_key"], "k");
        assert!(config.sources.login_for("pawoo.net").is_some());
        assert!(config.sources.login_for("notgelbooru.com").is_none());
    }

    #[test]
    fn x_reads_through_fxembed_unless_turned_off() {
        assert_eq!(
            Config::default().sources.x.fxembed_api_url,
            "https://api.fixupx.com"
        );
        let mut config = valid();
        config.sources.x.fxembed_api_url = String::new();
        assert!(config.validate().is_ok());
        config.sources.x.fxembed_api_url = "fixupx.com".into();
        let problems = config.validate().unwrap_err();
        assert_eq!(problems[0].key, "sources.x.fxembed_api_url");
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
    fn checks_telemetry() {
        let mut config = valid();
        config.telemetry.metrics_bind = Some("127.0.0.1:9100".parse().unwrap());
        config.telemetry.otlp_endpoint = Some(Url::parse("http://collector:4318").unwrap());
        config.telemetry.otlp_sample_ratio = 0.25;
        config.validate().unwrap();
        config.telemetry.metrics_bind = Some(config.server.bind);
        config.telemetry.otlp_endpoint = Some(Url::parse("grpc://collector:4317").unwrap());
        config.telemetry.otlp_sample_ratio = 2.0;
        let keys: Vec<_> = config
            .validate()
            .unwrap_err()
            .iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(
            keys,
            [
                "telemetry.otlp_endpoint",
                "telemetry.otlp_sample_ratio",
                "telemetry.metrics_bind"
            ]
        );
    }

    #[test]
    fn originals_lose_their_metadata_unless_turned_off() {
        assert_eq!(Config::default().media.strip_metadata, StripMetadata::Strip);
        let unset: Config = toml::from_str("[media]\nmax_upload_mb = 50\n").unwrap();
        assert_eq!(unset.media.strip_metadata, StripMetadata::Strip);
        let off: Config = toml::from_str("[media]\nstrip_metadata = \"off\"\n").unwrap();
        assert_eq!(off.media.strip_metadata, StripMetadata::Off);
    }

    #[test]
    fn ffmpeg_memory_is_off_or_enough_to_start() {
        let mut config = valid();
        config.media.ffmpeg_memory_mb = 0;
        config.validate().unwrap();
        config.media.ffmpeg_memory_mb = 100;
        let problems = config.validate().unwrap_err();
        assert_eq!(problems[0].key, "media.ffmpeg_memory_mb");
    }

    #[test]
    fn checks_cors_origins() {
        let mut config = valid();
        config.server.cors.allowed_origins = vec![
            "https://app.example.com".into(),
            "http://localhost:5173".into(),
        ];
        config.server.cors.allow_credentials = true;
        config.validate().unwrap();

        for bad in ["https://app.example.com/", "app.example.com", "ftp://x.org"] {
            config.server.cors.allowed_origins = vec![bad.into()];
            let problems = config.validate().unwrap_err();
            assert_eq!(problems[0].key, "server.cors.allowed_origins", "{bad}");
        }

        config.server.cors.allowed_origins = vec!["*".into()];
        let problems = config.validate().unwrap_err();
        assert_eq!(problems[0].key, "server.cors.allow_credentials");
        config.server.cors.allow_credentials = false;
        config.validate().unwrap();
    }

    #[test]
    fn checks_the_tagger_model() {
        let mut config = valid();
        let preset = config.tagger.source().unwrap();
        assert_eq!(preset.name, "wd-vit-tagger-v3");
        assert!(preset.model_url.as_str().ends_with("/model.onnx"));

        config.tagger.model = "wd-nonexistent".into();
        config.tagger.account = "no spaces allowed".into();
        let keys: Vec<_> = config
            .validate()
            .unwrap_err()
            .iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(keys, ["tagger.model", "tagger.account"]);

        let mut config = valid();
        config.tagger.model = "custom".into();
        config.tagger.model_url = Some(Url::parse("ftp://example.com/m.onnx").unwrap());
        config.tagger.model_sha256 = "abc".into();
        let keys: Vec<_> = config
            .validate()
            .unwrap_err()
            .iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(
            keys,
            [
                "tagger.model_url",
                "tagger.tags_url",
                "tagger.model_sha256",
                "tagger.tags_sha256"
            ]
        );
        assert!(config.tagger.source().is_none());

        config.tagger.model_url = Some(Url::parse("https://example.com/m.onnx").unwrap());
        config.tagger.tags_url = Some(Url::parse("https://example.com/t.csv").unwrap());
        config.tagger.model_sha256 = "AB".repeat(32);
        config.tagger.tags_sha256 = "cd".repeat(32);
        config.validate().unwrap();
        let custom = config.tagger.source().unwrap();
        assert_eq!(custom.model_sha256, "ab".repeat(32));
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
    fn redacted_hides_every_credential() {
        let config: Config = toml::from_str(
            r#"
            [database]
            url = "postgres://moekura:s3cr3t-db@db/moekura"
            replicas = ["postgres://db/moekura?password=s3cr3t-replica"]
            [cache]
            url = "rediss://default:s3cr3t-cache@valkey:6379"
            [auth.oidc]
            issuer = "https://sso.example.com"
            client_id = "moekura"
            client_secret = "s3cr3t-oidc"
            [auth.captcha]
            provider = "turnstile"
            site_key = "public-site-key"
            secret_key = "s3cr3t-captcha"
            verify_url = "https://verify:s3cr3t-verify@captcha.example.com/check?key=s3cr3t-verify-key"
            [mail]
            host = "smtp.example.com"
            username = "moekura"
            password = "s3cr3t-mail"
            [sources.logins."pixiv.net"]
            cookie = "PHPSESSID=s3cr3t-cookie"
            [sources.logins."gelbooru.com"]
            query = { user_id = "s3cr3t-user-id", api_key = "s3cr3t-api-key" }
            [sources.logins."pawoo.net"]
            headers = { Authorization = "Bearer s3cr3t-bearer" }
            [sources.x]
            fxembed_api_url = "https://fx:s3cr3t-fx@fx.example.com/?token=s3cr3t-fx-token"
            [storage]
            public_base_url = "https://cdn.example.com/?sig=s3cr3t-cdn"
            [storage.s3]
            endpoint = "https://minio:s3cr3t-endpoint@minio.example.com"
            secret_access_key = "s3cr3t-s3"
            [tagger]
            model = "custom"
            model_url = "https://huggingface.co/m/model.onnx?token=s3cr3t-model"
            tags_url = "https://user:s3cr3t-tags@example.com/tags.csv"
            [telemetry]
            otlp_endpoint = "https://otel:s3cr3t-otlp@collector.example.com:4318"
            "#,
        )
        .unwrap();
        let printed = toml::to_string_pretty(&config.redacted()).unwrap();
        assert!(!printed.contains("s3cr3t"), "{printed}");
        // Still says what is configured, and where.
        for shown in [
            "public-site-key",
            "Authorization",
            "api_key",
            "collector.example.com",
            "token=REDACTED",
        ] {
            assert!(printed.contains(shown), "{shown}: {printed}");
        }
        // Unset secrets stay visibly unset, and plain addresses unchanged.
        let plain = Config::default().redacted();
        assert_eq!(plain.mail.password, "");
        assert_eq!(plain.sources.x.fxembed_api_url, "https://api.fixupx.com");
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
