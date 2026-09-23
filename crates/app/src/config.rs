//! Loads [`Config`] from built-in defaults, a TOML file, then `UWU_*`
//! environment variables, each layer overriding the one before.

use std::path::Path;

use anyhow::{Context, bail};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use uwu_core::config::Config;

/// Read from the working directory when no path is given, if it exists.
pub const DEFAULT_PATH: &str = "uwubooru.toml";

/// Environment variables map to keys with `__` as the section separator:
/// `UWU_DATABASE__URL` sets `database.url`.
const ENV_PREFIX: &str = "UWU_";

pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
    let mut figment = Figment::from(Serialized::defaults(Config::default()));
    match path {
        Some(path) if !path.is_file() => bail!("config file {} does not exist", path.display()),
        Some(path) => figment = figment.merge(Toml::file_exact(path)),
        // Environment-only setups (e.g. containers) have no file at all.
        None if Path::new(DEFAULT_PATH).is_file() => {
            figment = figment.merge(Toml::file_exact(DEFAULT_PATH))
        }
        None => {}
    }
    let config: Config = figment
        // UWU_CONFIG selects the file itself and is not a config key.
        .merge(Env::prefixed(ENV_PREFIX).split("__").ignore(&["config"]))
        .extract()
        .context("could not load configuration")?;

    if let Err(problems) = config.validate() {
        let list: Vec<String> = problems.iter().map(|p| format!("  - {p}")).collect();
        bail!("invalid configuration:\n{}", list.join("\n"));
    }
    Ok(config)
}

#[cfg(test)]
// `Jail` closures must return `figment::Result`, whose error type is large.
#[allow(clippy::result_large_err)]
mod tests {
    use figment::Jail;

    use super::*;

    fn load_in_jail(path: Option<&Path>) -> figment::Result<Config> {
        load(path).map_err(|e| format!("{e:#}").into())
    }

    #[test]
    fn env_overrides_file_overrides_defaults() {
        Jail::expect_with(|jail| {
            jail.create_file(
                DEFAULT_PATH,
                r#"
                [database]
                url = "postgres://file/uwu"
                max_connections = 4
                "#,
            )?;
            jail.set_env("UWU_DATABASE__URL", "postgres://env/uwu");
            jail.set_env("UWU_CONFIG", "ignored.toml");

            let config = load_in_jail(None)?;
            assert_eq!(config.database.url, "postgres://env/uwu");
            assert_eq!(config.database.max_connections, 4);
            assert_eq!(config.server.request_timeout_secs, 30);
            Ok(())
        });
    }

    #[test]
    fn env_alone_is_enough() {
        Jail::expect_with(|jail| {
            jail.set_env("UWU_DATABASE__URL", "postgres://env/uwu");
            jail.set_env(
                "UWU_DATABASE__REPLICAS",
                r#"["postgres://r1/uwu", "postgres://r2/uwu"]"#,
            );
            jail.set_env("UWU_TELEMETRY__LOG_FORMAT", "json");

            let config = load_in_jail(None)?;
            assert_eq!(config.database.replicas.len(), 2);
            assert_eq!(
                config.telemetry.log_format,
                uwu_core::config::LogFormat::Json
            );
            Ok(())
        });
    }

    #[test]
    fn explicit_missing_file_is_an_error() {
        Jail::expect_with(|_| {
            let err = load_in_jail(Some(Path::new("nope.toml"))).unwrap_err();
            assert!(err.to_string().contains("does not exist"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn reports_validation_problems() {
        Jail::expect_with(|_| {
            let err = load_in_jail(None).unwrap_err();
            assert!(err.to_string().contains("database.url"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_unknown_keys() {
        Jail::expect_with(|jail| {
            jail.set_env("UWU_DATABASE__URL", "postgres://env/uwu");
            jail.set_env("UWU_DATABSE__URL", "typo");
            let err = load_in_jail(None).unwrap_err();
            assert!(err.to_string().contains("databse"), "{err}");
            Ok(())
        });
    }
}
