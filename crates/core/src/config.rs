//! Infrastructure configuration.
//!
//! These settings come from `uwuubooru.toml` and `UWUU_*` environment
//! variables and require a restart to change. Site settings that admins edit
//! at runtime live in the database instead.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub telemetry: TelemetryConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the HTTP server listens on.
    pub bind: SocketAddr,
    /// Requests running longer than this are aborted with `408`.
    pub request_timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080)),
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
    /// turn this off and run `uwuubooru migrate` as a separate release step.
    pub auto_migrate: bool,
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
        }
    }
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
                message: "is required (set it in the config file or UWUU_DATABASE__URL)".into(),
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
        Ok(url) if matches!(url.scheme(), "postgres" | "postgresql") => url,
        _ => return format!("<not a postgres URL, {REDACTED}>"),
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
        config.database.url = "postgres://uwuu:hunter2@localhost/uwuu".into();
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
        config.database.url = "mysql://localhost/uwuu".into();
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
            redact_url("postgres://uwuu:hunter2@db:5432/uwuu"),
            "postgres://uwuu:REDACTED@db:5432/uwuu"
        );
    }

    #[test]
    fn redacts_query_password() {
        assert_eq!(
            redact_url("postgres://db/uwuu?user=uwuu&password=hunter2&sslmode=require"),
            "postgres://db/uwuu?user=uwuu&password=REDACTED&sslmode=require"
        );
    }

    #[test]
    fn leaves_passwordless_urls_alone() {
        assert_eq!(
            redact_url("postgres://uwuu@db/uwuu"),
            "postgres://uwuu@db/uwuu"
        );
        assert_eq!(redact_url(""), "");
    }

    #[test]
    fn replaces_non_postgres_urls() {
        // Parses as a URL with scheme `uwuu`, so only the scheme check stops the leak.
        assert!(!redact_url("uwuu:hunter2 garbage").contains("hunter2"));
        assert!(!redact_url("not even a url hunter2").contains("hunter2"));
        assert!(!redact_url("mysql://u:hunter2@db/x").contains("hunter2"));
    }

    #[test]
    fn redacted_masks_replicas() {
        let mut config = valid();
        config.database.replicas = vec!["postgres://r:secret@replica/uwuu".into()];
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
